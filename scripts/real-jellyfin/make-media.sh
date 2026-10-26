#!/usr/bin/env bash
# Generate the small H.264/AAC test media for three Jellyfin servers: some
# titles unique to one server, some duplicated (with a different resolution
# on server 2, so the dedupe winner is observable), and one show whose
# episodes are split across servers 1 and 2.
# usage: make-media.sh [media-root]        (default /srv/jf/media)
set -euo pipefail
ROOT=${1:-/srv/jf/media}
gen() { # gen <file> <WxH>: 8 s test pattern + 440 Hz tone, keyframe every 2 s
  mkdir -p "$(dirname "$1")"
  [ -f "$1" ] || ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=$2:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" -t 8 -c:v libx264 -preset veryfast \
    -pix_fmt yuv420p -g 48 -c:a aac -b:a 64k -shortest "$1"
}
mv_() { gen "$ROOT/$1/Movies/$2/$2.mkv" "${3:-320x180}"; }
ep() { gen "$ROOT/$1/Shows/$2/Season $(printf %02d "$3")/${2% (*} S$(printf %02d "$3")E$(printf %02d "$4").mkv" 320x180; }

# server 1
mv_ s1 "Big Buck Bunny (2008)"
mv_ s1 "Sintel (2010)"
mv_ s1 "Night of the Living Dead (1968)"
gen "$ROOT/s1/Kids/Elephants Dream (2006)/Elephants Dream (2006).mkv" 320x180
ep s1 "Breaking Bad (2008)" 1 1
ep s1 "Breaking Bad (2008)" 1 2
ep s1 "The Office (US) (2005)" 1 1
srt="$ROOT/s1/Movies/Big Buck Bunny (2008)/Big Buck Bunny (2008).en.srt"
[ -f "$srt" ] || printf '1\n00:00:01,000 --> 00:00:04,000\nHello from the proxy test\n\n2\n00:00:05,000 --> 00:00:07,000\nSecond line\n' >"$srt"
# server 2: duplicates at 640x360, unique movie, more Breaking Bad episodes
mv_ s2 "Big Buck Bunny (2008)" 640x360
mv_ s2 "Sintel (2010)" 640x360
mv_ s2 "Tears of Steel (2012)"
ep s2 "Breaking Bad (2008)" 1 3
ep s2 "Breaking Bad (2008)" 2 1
# server 3: one duplicate, one unique movie, and a "Shows" library of type
# movies (same name, different type: must not merge with the TV "Shows")
mv_ s3 "Big Buck Bunny (2008)"
mv_ s3 "Charade (1963)"
gen "$ROOT/s3/Shows/Plan 9 from Outer Space (1957)/Plan 9 from Outer Space (1957).mkv" 320x180
find "$ROOT" -type f | sort
