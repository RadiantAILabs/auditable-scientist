#!/bin/sh
# Stand-in for pdflatex. It reads marker comments in paper.tex instead of
# typesetting, so tests can choose the outcome of a build:
#
#   % fake: fail                 exit 1 after writing a TeX-style error to the log
#   % fake: hang                 start a background child and wait for 60 seconds
#   % fake: pages N              pages reported in the log (default 9)
#   % fake: refs PAGE TOTAL GOAL where the bibliography begins, in TeX points
#                                (default: page 9, 420pt of 550pt)
#   % fake: nopdf                exit 0 without writing a PDF
#   % fake: quiet                leave no call record in the folder
#
# Every run appends its arguments to fake_calls.txt and the names of its
# environment variables to fake_env.txt, unless the paper asks for quiet.
set -u

tex=paper.tex
log=paper.log
marker() {
  sed -n "s/^% fake: $1\$/yes/p;s/^% fake: $1 \(.*\)\$/\1/p" "$tex" | head -n 1
}

if [ -z "$(marker quiet)" ]; then
  printf 'pdflatex %s\n' "$*" >> fake_calls.txt
  env | cut -d= -f1 | sort >> fake_env.txt
fi

if [ -n "$(marker hang)" ]; then
  sleep 60 &
  echo "$!" > fake_child.pid
  wait
  exit 0
fi

if [ -n "$(marker fail)" ]; then
  {
    echo "This is FakeTeX"
    echo "./paper.tex:7: Undefined control sequence."
    printf '%s\n' 'l.7 \nonexistent'
  } > "$log"
  exit 1
fi

pages=$(marker pages)
pages=${pages:-9}
refs=$(marker refs)
refs=${refs:-9 420 550}
set -- $refs

{
  echo "This is FakeTeX"
  echo "PSPROBE refs page=$1; total=$2pt; goal=$3pt;"
  if grep -q '\\appendix' "$tex"; then
    echo "PSPROBE appendix page=$pages; total=0.0pt; goal=16383.99998pt;"
  fi
  if [ -z "$(marker nopdf)" ]; then
    echo "Output written on paper.pdf ($pages pages, 1234 bytes)."
  fi
} > "$log"

if grep -q '\\bibliography{' "$tex"; then
  printf '%s\n' '\bibdata{references}' > paper.aux
else
  : > paper.aux
fi

if [ -z "$(marker nopdf)" ]; then
  printf '%s\n' '%PDF-1.5 fake' > paper.pdf
fi
exit 0
