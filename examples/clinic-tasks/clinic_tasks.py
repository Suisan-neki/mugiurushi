"""架空の診療所の連絡タスク管理（デモ用）。実在の診療所・患者・職員のデータは扱わない。

職員がコマンドで、患者ごとの連絡タスクを追加し、一覧で読み直す。
  python3 clinic_tasks.py --dir DIR --user USER add PATIENT TEXT
  python3 clinic_tasks.py --dir DIR --user USER list
"""

import argparse
import os
import sys

# 担当者だけが自分の登録したタスクを読めるようにするか。
RESTRICT_TO_OWNER = False
# 患者の情報への操作を記録するか。
ACCESS_LOG = False


def log(directory, user, action, patient):
    if ACCESS_LOG:
        with open(os.path.join(directory, "access.log"), "a", encoding="utf-8") as f:
            f.write(f"user={user} action={action} patient={patient}\n")


def main(argv):
    parser = argparse.ArgumentParser(description="架空の診療所の連絡タスク")
    parser.add_argument("--dir", default=".", help="保存先のディレクトリ")
    parser.add_argument("--list", default="tasks.tsv", help="保存するファイルの名前")
    parser.add_argument("--user", required=True, help="操作する職員のID")
    sub = parser.add_subparsers(dest="command", required=True)
    add = sub.add_parser("add", help="タスクを追加する")
    add.add_argument("patient")
    add.add_argument("text")
    sub.add_parser("list", help="タスクを一覧で表示する")
    args = parser.parse_args(argv)
    path = os.path.join(args.dir, args.list)
    if args.command == "add":
        with open(path, "a", encoding="utf-8") as f:
            f.write(f"{args.user}\t{args.patient}\t{args.text}\n")
        log(args.dir, args.user, "add", args.patient)
        print(f"追加しました: {args.patient}")
        return 0
    if not os.path.exists(path):
        print("タスクはありません")
        return 0
    with open(path, encoding="utf-8") as f:
        for line in f:
            owner, patient, text = line.rstrip("\n").split("\t", 2)
            if RESTRICT_TO_OWNER and owner != args.user:
                continue
            log(args.dir, args.user, "read", patient)
            print(f"{patient}: {text}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
