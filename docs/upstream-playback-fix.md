# Upstream playback compatibility

Based on liwanspecial/HGhub-web commit c1c3f020acf1fba243eb2bd8d1b4c89f2e1abb35
and its referenced zxsos/HGhub playback fix.

Both the native client and local web service now request episode playback using
the upstream POST playback API. The old GET response may succeed with no URL.
Signed addresses and CDN credentials stay in bounded, identity-bound in-memory
tickets. All nested HLS resources use local proxy URLs, and credentials are
limited to the original CDN origin. Account or line changes invalidate tickets.
Native protocol responses retain Range and have a 64 MiB buffering limit.

Explicit retry requests new playback credentials; one automatic recovery is
allowed after media failure. Resume refresh preserves playback position.

Encrypted DNS is enabled by default to avoid the observed poisoned local DNS
answers. It uses HTTPS to dns.google through the configured system proxy or
HGHUB_PROXY and still validates all A/AAAA results using the existing target
policy. Set HGHUB_DOH=0 to use the system resolver instead. It does not change
system DNS, proxy, or security settings.

Windows verification runs in .github/workflows/windows-check.yml and publishes
a private workflow artifact, without creating a public release.
See README-web.md for the independent local web service.
