# Download current episode as MP4

The user selected a single MP4 file for computer and phone playback. Add a Windows
native “下载本集” action, a background task indicator with progress/cancel, and
save results under the user's Downloads/HGhub directory. Keep one active download.
The browser edition stays unchanged.

Approaches: raw HLS packages need a compatible player; MP4 remux via FFmpeg gives
a single widely usable file without re-encoding. The selected approach is MP4.
Use a separately installed FFmpeg companion, with an installer script and an
explicit missing-component error. Do not commit or redistribute third-party binaries.

FFmpeg only reads a short-lived capability URL on loopback. Requests go through
the existing identity-bound TicketStore and guarded media proxy, including HLS
keys/segments. Upstream credentials stay out of process arguments and logs.
Keep protocols restricted, run without a shell or console, preserve existing files,
write a unique .part.mp4, and expose the final name only after successful completion.
Cancel or failure terminates the process/server and removes partial output.

No batches, no automatic episode traversal. All implementation and tests stay in
the background: no Playwright, no front window control, and no live media downloads
for testing. Use synthetic HLS with FFmpeg/ffprobe for verification.
