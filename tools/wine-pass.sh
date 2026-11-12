#!/bin/sh
# Like-a-human interactive verification pass for the Windows exe under
# Wine, per DESIGN.md §7 and AGENTS.md. Run from the repo root.
#
# Prerequisites (see AGENTS.md "verification tooling"):
#   gcc /tmp/opencode/xgrab.c -o /tmp/opencode/xgrab -lX11
#   gcc /tmp/opencode/xtest.c -o /tmp/opencode/xtest -lX11 -lXtst
#   python3 + PIL for the RGB->PNG step.
#
# Usage: tools/wine-pass.sh [path/to/file.parquet]
# The script starts a nested Xwayland (:99), launches the exe, and then
# drives one interaction per step, pausing for the operator (or the AI
# harness) to grab screenshots between steps. Steps print their expected
# result so failures are attributable.

set -e
REPO=$(pwd)
PARQUET=${1:-dist/test-data.parquet}
XGRAB=/tmp/opencode/xgrab
XTEST=/tmp/opencode/xtest
OUT=${OUT:-/tmp/opencode/pass}

# RGB -> PNG helper (PIL-free fallback included).
topng() { python3 /tmp/opencode/topng.py "$1" "$2"; }

grab() { # grab <wid> <name>
    "$XGRAB" "$1" "$OUT/$2.rgb" >/dev/null && topng "$OUT/$2.rgb" "$OUT/$2.png"
}

step() { echo "== $1"; }

mkdir -p "$OUT"

step "cleanup: kill leftovers"
wineserver -k 2>/dev/null || true
pkill -9 -f "Xwayland :99" 2>/dev/null || true
sleep 1

step "start nested Xwayland :99"
(DISPLAY=:0 setsid nohup Xwayland :99 -geometry 1280x800 -nolisten tcp >/dev/null 2>&1 &)
sleep 3

step "launch exe: $PARQUET (expect: table renders, title has build stamp)"
(DISPLAY=:99 setsid nohup wine "$REPO/dist/pqeditor.exe" "$PARQUET" >/dev/null 2>&1 &)
sleep 9
WID=$(DISPLAY=:99 xwininfo -root -tree | grep 1280x800 | grep -oE "0x[0-9a-f]+" | head -1)
echo "WID=$WID"
"$XTEST" moveresize "$WID" 0 0 1270 790
"$XTEST" focus "$WID"
sleep 0.5
# NOTE: after moveresize the app's cached window origin is stale by the
# OLD offset (+4,+30): to click at app coords (x,y) use (x+4, y+30).

step "1. header sort cycle (expect: asc -> desc -> off, no blank table)"
"$XTEST" click 254 81; sleep 1
grab "$WID" s1_asc
"$XTEST" click 254 81; sleep 1
grab "$WID" s1_desc

step "2. edit under sort via Enter (expect: editor opens and STAYS)"
"$XTEST" click 254 121; sleep 0.4
"$XTEST" key Return; sleep 0.8
grab "$WID" s2_editor
"$XTEST" type "1"; "$XTEST" key Return; sleep 0.5
grab "$WID" s2_committed

step "3. save (expect: NO flicker, sort intact, status 'saved')"
"$XTEST" combo ctrl s; sleep 2
grab "$WID" s3_saved

step "4. copy 2x2, paste (expect: values land at the target, all-or-nothing)"
"$XTEST" drag 254 141 404 200; sleep 0.5
grab "$WID" s4_drag
"$XTEST" combo ctrl c; sleep 0.4
"$XTEST" click 254 254; sleep 0.4
"$XTEST" combo ctrl v; sleep 0.6
grab "$WID" s4_pasted

step "5. delete a row (rownum click + Del, expect: count -1, 'rows deleted')"
"$XTEST" click 61 113; sleep 0.4
"$XTEST" key Delete; sleep 1
grab "$WID" s5_deleted

step "6. save (expect: count stable, no re-render)"
"$XTEST" combo ctrl s; sleep 2
grab "$WID" s6_saved

step "7. filter box (expect: shown count matches; empty submit clears)"
"$XTEST" combo ctrl l
"$XTEST" type "id > 90"
"$XTEST" key Return; sleep 1
grab "$WID" s7_filtered
"$XTEST" combo ctrl l; "$XTEST" combo ctrl a; "$XTEST" key Delete
"$XTEST" key Return; sleep 0.8
grab "$WID" s7_cleared

step "8. export dialog (expect: 480px-wide modal, stable under input)"
"$XTEST" combo ctrl e; sleep 1
grab "$WID" s8_export

step "9. quit (expect: unsaved-changes guard when dirty)"
sleep 0.5

step "cleanup"
wineserver -k 2>/dev/null || true
pkill -9 -f "Xwayland :99" 2>/dev/null || true
echo "done — review $OUT/*.png"
