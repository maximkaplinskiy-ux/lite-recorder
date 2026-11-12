"""Optional QA utility: python -m pip install av, then inspect recordings.

PyAV is only used for development verification, never by the recorder.
"""
import json
import math
import struct
import sys
import wave
from pathlib import Path

if sys.argv[1] == "tone":
    with wave.open(sys.argv[2], "wb") as out:
        out.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
        out.writeframes(b"".join(struct.pack("<hh", *([int(1000 * math.sin(2*math.pi*440*i/48000))]*2)) for i in range(48000)))
    sys.exit()

import av

for name in sys.argv[1:]:
    path = Path(name)
    report = {"file": str(path), "bytes": path.stat().st_size, "streams": []}
    with av.open(str(path)) as container:
        report["duration_seconds"] = container.duration / av.time_base
        assert len(container.streams.video) == 1
        assert len(container.streams.audio) == 1
        for stream in container.streams:
            report["streams"].append({"type": stream.type, "codec": stream.codec_context.name, "duration": float(stream.duration * stream.time_base) if stream.duration else None})
    for kind in ["video", "audio"]:
        count, peak, first, last = 0, 0, None, None
        with av.open(str(path)) as container:
            for frame in container.decode(**{kind: 0}):
                count += 1
                first = frame.time if first is None else first
                last = frame.time
                if kind == "audio":
                    plane = memoryview(frame.planes[0]).cast("f")
                    peak = max(peak, max((abs(v) for v in plane[:frame.samples]), default=0))
                elif count == 1:
                    frame.to_image().save(str(path.with_suffix(".first-frame.png")))
        report[kind] = {"decoded_frames": count, "first_seconds": first, "last_seconds": last}
        assert count > 0, f"No {kind} frames decoded"
        if kind == "audio":
            report[kind]["peak"] = peak
    print(json.dumps(report, ensure_ascii=False, indent=2))
