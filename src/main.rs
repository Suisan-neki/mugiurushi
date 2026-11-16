mod cli;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    // 終了の合図を受けたら、試験用の資源を回収してから終わる。中継として動く場合は、資源を持たないので設定しない。
    if args.first().map(String::as_str) != Some("relay")
        && let Err(error) = mugiurushi::shutdown::install()
    {
        eprintln!("エラー: 終了の合図を受ける設定ができません: {error}");
        std::process::exit(1);
    }
    let result = cli::run(&args);
    // 合図の回収が始まっていたら、ここで終えずに、回収のスレッドが合図の終了コードで終えるのを待つ。
    mugiurushi::shutdown::wait_if_stopping();
    if let Err(error) = result {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}
