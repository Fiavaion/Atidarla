# HAP test clips

Generated with FFmpeg from its built-in `testsrc2` pattern (64×32, 8 fps,
1 s), so they carry no third-party content. `*.f2.rgba` is FFmpeg's own
decode of frame 2 (t = 0.25 s) as raw RGBA, the reference for the decoder
tests.

```sh
SRC='testsrc2=size=64x32:rate=8:duration=1'
ffmpeg -f lavfi -i "$SRC" -c:v hap -format hap hap1.mov
ffmpeg -f lavfi -i "$SRC" -vf "format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='255*X/W'" -c:v hap -format hap_alpha hap_alpha.mov
ffmpeg -f lavfi -i "$SRC" -c:v hap -format hap_q hap_q.mov
ffmpeg -f lavfi -i "$SRC" -c:v hap -format hap -chunks 4 hap1_chunked.mov
ffmpeg -f lavfi -i "$SRC" -c:v hap -format hap -compressor none hap1_raw.mov
for f in hap1 hap_alpha hap_q hap1_chunked hap1_raw; do
  ffmpeg -i $f.mov -frames:v 1 -ss 0.25 -f rawvideo -pix_fmt rgba $f.f2.rgba
done
```
