"""架空の診療所の問診（デモ用）。実在の診療所・患者・職員のデータは扱わない。

患者がコマンドで問診に答え、職員が回答を読む。回答は患者ごとのファイルに保存する。
  python3 monshin.py --dir DIR --user USER answer PATIENT TEXT   問診に答える（患者本人）
  python3 monshin.py --dir DIR --user USER show PATIENT          回答を表示する（職員）

利用者（架空）:
  P001, P002        患者
  doctor_d          医師
  reception_b       受付
"""

import argparse
import os
import sys

USERS = {
    "P001": "patient",
    "P002": "patient",
    "doctor_d": "doctor",
    "reception_b": "reception",
}

# 問診の回答を開ける職種。
STAFF = {"doctor", "reception"}
# 回答の内容まで読める職種。それ以外の職員には、回答があるかどうかだけを示す。
CAN_READ_ANSWERS = {"doctor", "reception"}


def answer_path(directory, patient):
    return os.path.join(directory, "answers", f"{patient}.tsv")


def main(argv):
    parser = argparse.ArgumentParser(description="架空の診療所の問診")
    parser.add_argument("--dir", default=".", help="保存先のディレクトリ")
    parser.add_argument("--user", required=True, help="操作する人のID")
    sub = parser.add_subparsers(dest="command", required=True)
    answer = sub.add_parser("answer", help="問診に答える")
    answer.add_argument("patient")
    answer.add_argument("text")
    show = sub.add_parser("show", help="問診の回答を表示する")
    show.add_argument("patient")
    args = parser.parse_args(argv)

    role = USERS.get(args.user)
    if role is None:
        print(f"登録されていない利用者です: {args.user}", file=sys.stderr)
        return 2

    if args.command == "answer":
        if role != "patient" or args.user != args.patient:
            print("問診には患者本人だけが答えられます", file=sys.stderr)
            return 3
        path = answer_path(args.dir, args.patient)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a", encoding="utf-8") as f:
            f.write(f"{args.patient}\t{args.text}\n")
        print(f"回答を保存しました: {args.patient}")
        return 0

    if role not in STAFF:
        print("この利用者は問診の回答を開けません", file=sys.stderr)
        return 3
    path = answer_path(args.dir, args.patient)
    if not os.path.exists(path):
        print("回答はありません")
        return 0
    with open(path, encoding="utf-8") as f:
        for line in f:
            patient, text = line.rstrip("\n").split("\t", 1)
            if role in CAN_READ_ANSWERS:
                print(f"{patient}: {text}")
            else:
                print(f"{patient}: 回答あり（内容は医師だけが読めます）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
