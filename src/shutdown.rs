//! 終了の合図（SIGTERM、SIGINT）を受けたときに、試験用の資源を回収してから終わる。
//!
//! 通常の回収は、値の破棄（`Drop`）に頼っている。合図で終わるプロセスでは破棄の処理が動かず、
//! 子のプロセスグループと、試行のコンテナ・ネットワーク・ボリュームが残る。
//! ここでは、動いている資源を控えておき、合図を受けたら、控えた資源だけを回収する。
//!
//! 合図を受ける関数の中では、パイプへ1バイト書くことだけを行う。回収は、パイプを読む別のスレッドが行う。
//! 2回目の合図では、回収を待たずに終わる。
//!
//! 回収は、試行を動かしているスレッドとは別のスレッドで動く。回収の後に試行の側が資源を作ると、その資源が残る。
//! そこで、資源を作る操作は `creating` を通す。回収は、作っている途中の操作が終わるのを待ってから始め、
//! 回収を始めた後の `creating` は、何も作らずにエラーを返す。
use std::{
    collections::BTreeSet,
    io,
    process::{Command, Output, Stdio},
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

/// 回収の対象として控えている資源。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    /// 専用のプロセスグループで起動した子のPID。
    groups: BTreeSet<u32>,
    /// コンテナの試行のラベル（`eel.trial` の値）。
    trials: BTreeSet<String>,
    /// ラベルを持たない、名前で指すコンテナ。
    containers: BTreeSet<String>,
    /// 実行中の `docker` コマンドのPID。専用のグループを持たない。
    commands: BTreeSet<u32>,
}

static HELD: Mutex<Held> = Mutex::new(Held {
    groups: BTreeSet::new(),
    trials: BTreeSet::new(),
    containers: BTreeSet::new(),
    commands: BTreeSet::new(),
});
/// 資源を作る操作が読む側、回収が書く側で取る。回収は、作っている途中の操作が終わるまで待つ。
static CREATING: RwLock<()> = RwLock::new(());
static STOPPING: AtomicBool = AtomicBool::new(false);
static SIGNALS: AtomicUsize = AtomicUsize::new(0);
static PIPE: AtomicI32 = AtomicI32::new(-1);

fn held<T>(change: impl FnOnce(&mut Held) -> T) -> T {
    // ほかのスレッドが途中で終わっていても、控えは使う。回収できないより、古い控えで回収するほうがよい。
    let mut guard = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    change(&mut guard)
}

/// 資源（子のプロセス、コンテナ、ネットワーク、ボリューム）を作る操作を、回収と重ならないように行う。
/// 回収を始めた後は、何も作らずにエラーを返す。作った資源を控える処理も、この中で行う。
pub(crate) fn creating<T>(create: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let _guard = CREATING
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if stopping() {
        return Err(io::Error::other(
            "終了の合図を受けたので、新しい資源を作らない",
        ));
    }
    create()
}

/// コマンドを実行して出力を受け取る。実行中のPIDを控え、合図を受けたときに止められるようにする。
/// 標準出力と標準エラーは受け取る。標準入力の扱いは、呼び出し側が決める。
pub(crate) fn output(command: &mut Command) -> io::Result<Output> {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    hold_command(pid);
    let output = child.wait_with_output();
    release_command(pid);
    output
}

pub(crate) fn hold_command(pid: u32) {
    held(|h| h.commands.insert(pid));
}
pub(crate) fn release_command(pid: u32) {
    held(|h| h.commands.remove(&pid));
}
pub(crate) fn hold_group(pid: u32) {
    held(|h| h.groups.insert(pid));
}
pub(crate) fn release_group(pid: u32) {
    held(|h| h.groups.remove(&pid));
}
pub(crate) fn hold_trial(trial: &str) {
    held(|h| h.trials.insert(trial.to_owned()));
}
pub(crate) fn release_trial(trial: &str) {
    held(|h| h.trials.remove(trial));
}
pub(crate) fn hold_container(name: &str) {
    held(|h| h.containers.insert(name.to_owned()));
}
pub(crate) fn release_container(name: &str) {
    held(|h| h.containers.remove(name));
}

/// 終了の合図を受けて、回収を始めたか。回収の後で新しい資源を作らないために、起動の前に確かめる。
pub fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

/// 回収を始めていたら、回収のスレッドがプロセスを終えるまで待つ。処理の終わりに呼ぶ。
/// 回収が子のプロセスを止めると、試行の処理は「子が終わった」と見て先へ進み、通常の終了コードで終わってしまう。
/// 合図で終わったことが終了コードで分かるように、通常の終了より回収の終了を優先する。
pub fn wait_if_stopping() {
    while SIGNALS.load(Ordering::SeqCst) > 0 {
        thread::sleep(Duration::from_millis(50));
    }
}

/// 合図を受ける関数。行ってよいのは、合図の途中でも安全な操作だけである。
extern "C" fn on_signal(signal: libc::c_int) {
    if SIGNALS.fetch_add(1, Ordering::SeqCst) >= 1 {
        // SAFETY: `_exit` は合図の途中でも呼べる。
        unsafe { libc::_exit(128 + signal) };
    }
    let byte = signal as u8;
    // SAFETY: `write` は合図の途中でも呼べる。書き込み先は `install` が開いたパイプである。
    unsafe { libc::write(PIPE.load(Ordering::SeqCst), (&raw const byte).cast(), 1) };
}

/// 終了の合図を受けたら、控えた資源を回収してから終わるようにする。プロセスの最初に1回だけ呼ぶ。
pub fn install() -> io::Result<()> {
    let mut ends = [0 as libc::c_int; 2];
    // SAFETY: 2つの要素を持つ配列を渡している。
    if unsafe { libc::pipe(ends.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    for end in ends {
        // 子のプロセスへ、このパイプを引き継がない。
        // SAFETY: いま開いたファイル記述子だけを指定する。
        unsafe { libc::fcntl(end, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    PIPE.store(ends[1], Ordering::SeqCst);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: 構造体をゼロで初期化し、必要な項目を設定してから渡す。合図を受ける関数は、安全な操作だけを行う。
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&raw mut action.sa_mask);
            action.sa_flags = libc::SA_RESTART;
            if libc::sigaction(signal, &raw const action, std::ptr::null_mut()) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    thread::Builder::new()
        .name("shutdown".into())
        .spawn(move || {
            let mut byte = [0u8; 1];
            loop {
                // SAFETY: 1バイトの配列へ、1バイトだけ読む。
                let read = unsafe { libc::read(ends[0], byte.as_mut_ptr().cast(), 1) };
                if read == 1 {
                    break;
                }
                if read == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            let signal = libc::c_int::from(byte[0]);
            STOPPING.store(true, Ordering::SeqCst);
            // 作っている途中の資源ができあがるのを待つ。これより後は、新しい資源は作られない。
            drop(
                CREATING
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            let snapshot = held(|h| h.clone());
            // 資源を控えていないとき（試行を動かしていないとき）は、何も書かずに終わる。
            if snapshot != Held::default() {
                let report = collect(&snapshot);
                eprintln!(
                    "終了の合図（{}）を受けた。{report}",
                    if signal == libc::SIGINT {
                        "SIGINT"
                    } else {
                        "SIGTERM"
                    }
                );
            }
            std::process::exit(128 + signal);
        })?;
    Ok(())
}

fn signal_group(pid: u32, signal: libc::c_int) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: 専用のグループで起動した子の正のPIDだけを控えている。負のPIDは、そのグループを指す。
        unsafe { libc::kill(-pid, signal) };
    }
}

/// 控えた資源を回収し、行ったことを文で返す。
pub(crate) fn collect(held: &Held) -> String {
    // 子のプロセスグループ。先に終了を求める。ブラウザの操作器は、求められるとブラウザを閉じてから終わる。
    for pid in &held.groups {
        signal_group(*pid, libc::SIGTERM);
    }
    if !held.groups.is_empty() {
        thread::sleep(Duration::from_millis(1500));
    }
    for pid in &held.groups {
        signal_group(*pid, libc::SIGKILL);
    }
    let mut left = vec![];
    for name in &held.containers {
        if !crate::container::remove_named(name) {
            left.push(format!("container:{name}"));
        }
    }
    for trial in &held.trials {
        left.extend(crate::container::remove_labeled(trial));
    }
    // 試行の側が実行していた `docker` コマンドのうち、まだ動いているもの。
    // コンテナを消すと、ログの読み取りやコンテナの中のコマンドは終わる。イメージの取得などは残るので止める。
    let commands = self::held(|h| h.commands.clone());
    for pid in &commands {
        if let Ok(pid) = i32::try_from(*pid) {
            // SAFETY: 控えているのは、このプロセスが起動して、まだ終了を受け取っていない子の正のPIDである。
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
    format!(
        "子のプロセスグループ{}件と実行中のdockerコマンド{}件を止め、試行{}件とコンテナ{}件の資源を回収した。{}",
        held.groups.len(),
        commands.len(),
        held.trials.len(),
        held.containers.len(),
        if left.is_empty() {
            "残った資源はない".to_owned()
        } else {
            format!("残った資源: {}", left.join("、"))
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn alive(pid: u32) -> bool {
        // SAFETY: 合図0は、何も送らずにプロセスの有無だけを確かめる。
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[test]
    fn collecting_stops_the_held_process_group_and_its_descendants() {
        use std::os::unix::process::CommandExt;
        let directory =
            std::env::temp_dir().join(format!("mugiurushi-shutdown-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("grandchild");
        let _ = std::fs::remove_file(&marker);
        // 子が孫を起動し、孫のPIDを書いてから待つ。この試験の控えは、全体の控えとは別に作る。
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$1\"; wait")
            .arg("held")
            .arg(&marker)
            .process_group(0)
            .spawn()
            .unwrap();
        let grandchild = loop {
            if let Some(pid) = std::fs::read_to_string(&marker)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                break pid;
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(alive(child.id()) && alive(grandchild));
        let mut only = Held::default();
        only.groups.insert(child.id());
        let report = collect(&only);
        child.wait().unwrap();
        assert!(report.contains("子のプロセスグループ1件"), "{report}");
        assert!(report.contains("残った資源はない"), "{report}");
        // 孫も同じグループなので止まる。止まるまで少し待つ。
        let stopped = (0..50).any(|_| {
            thread::sleep(Duration::from_millis(20));
            !alive(grandchild)
        });
        assert!(stopped, "孫のプロセス {grandchild} が残っている");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn held_resources_are_recorded_and_released() {
        // 全体の控えを使う。ほかの試験と重ならない名前だけを足して、外す。
        hold_trial("shutdown-test-trial");
        hold_container("shutdown-test-container");
        assert!(held(|h| h.trials.contains("shutdown-test-trial")
            && h.containers.contains("shutdown-test-container")));
        release_trial("shutdown-test-trial");
        release_container("shutdown-test-container");
        assert!(held(|h| !h.trials.contains("shutdown-test-trial")
            && !h.containers.contains("shutdown-test-container")));
        assert!(!stopping());
    }
}
