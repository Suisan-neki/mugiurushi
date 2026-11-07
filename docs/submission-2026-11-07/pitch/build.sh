#!/bin/bash
# ピッチ用動画を組み立てる。元は無編集の録画 SRC。文字の画面とデモ場面をつなぐ。
set -e
SRC=$1; OUT=$2; cd "$(dirname "$0")"; mkdir -p seg; : > list.txt
card() { ffmpeg -loglevel error -y -loop 1 -t $2 -i card-$1.png -vf "fade=in:d=0.35,fade=out:st=$(echo "$2-0.35"|bc):d=0.35,format=yuv420p" -r 30 -c:v libx264 -crf 20 seg/$1.mp4; echo "file 'seg/$1.mp4'" >> list.txt; }
scene() { # id start end speed x y w h
  local id=$1 a=$2 b=$3 sp=$4 x=$5 y=$6 w=$7 h=$8
  local dur=$(echo "($b-$a)/$sp" | bc -l)
  ffmpeg -loglevel error -y -loop 1 -i bg-$id.png -ss $a -t $(echo "$b-$a"|bc) -i "$SRC" -filter_complex \
    "[1:v]setpts=PTS/$sp,crop=$w:$h:$x:$y,scale=1280:720:force_original_aspect_ratio=decrease:flags=lanczos,pad=1280:720:(ow-iw)/2:(oh-ih)/2:white[c];[0:v][c]overlay=40:118:shortest=1,fade=in:d=0.25,fade=out:st=$(echo "$dur-0.25"|bc -l):d=0.25,format=yuv420p" \
    -r 30 -t $dur -c:v libx264 -crf 20 seg/$id.mp4
  echo "file 'seg/$id.mp4'" >> list.txt; }
card hook 5
card problem 5
card solution 5.5
python3 - "$SRC" <<'PY' > scenes.sh
import json,sys
for s in json.load(open('scenes.json')):
    a,b=s['src']; x,y,w,h=s['crop']
    print(f"scene {s['id']} {a} {b} {s.get('speed',1)} {x} {y} {w} {h}")
PY
. ./scenes.sh
# 発見の場面は、実行結果（s4）の直後、一覧（s4b）の前に置く
grep -v "seg/s4b.mp4\|seg/s5.mp4" list.txt > l2.txt; mv l2.txt list.txt
card reveal 5
echo "file 'seg/s4b.mp4'" >> list.txt; echo "file 'seg/s5.mp4'" >> list.txt
card numbers 6
card close 6
ffmpeg -loglevel error -y -f concat -safe 0 -i list.txt -c copy "$OUT"
ffprobe -v error -show_entries format=duration -of csv=p=0 "$OUT"
