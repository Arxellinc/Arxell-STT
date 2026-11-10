import os
import subprocess
import wave
from array import array
from pathlib import Path

ROOT = str(Path(__file__).resolve().parent)
TOOL = Path(ROOT).parents[1] / ".local/tooling/espeak/root/usr"
ESPEAK = str(TOOL / "bin/espeak-ng")
ENV = dict(os.environ)
ENV["LD_LIBRARY_PATH"] = str(TOOL / "lib/x86_64-linux-gnu")
ENV["ESPEAK_DATA_PATH"] = str(TOOL / "lib/x86_64-linux-gnu/espeak-ng-data")

os.makedirs(ROOT, exist_ok=True)


def speak(text, name):
    path = os.path.join(ROOT, f"{name}.wav")
    subprocess.run(
        [ESPEAK, "-w", path, "-s", "140", "-v", "en-us", text],
        check=True,
        env=ENV,
    )
    return path


def read_wav(path):
    with wave.open(path, "rb") as wav:
        assert wav.getnchannels() == 1
        assert wav.getsampwidth() == 2
        rate = wav.getframerate()
        data = array("h")
        data.frombytes(wav.readframes(wav.getnframes()))
    return rate, list(data)


def resample(samples, src_rate, dst_rate=16000):
    if src_rate == dst_rate:
        return samples
    ratio = dst_rate / src_rate
    out_len = int(len(samples) * ratio)
    out = []
    for i in range(out_len):
        pos = i / ratio
        i0 = int(pos)
        i1 = min(i0 + 1, len(samples) - 1)
        frac = pos - i0
        out.append(int(samples[i0] * (1 - frac) + samples[i1] * frac))
    return out


def silence(ms):
    return [0] * int(16000 * ms / 1000)


def write_wav(path, samples):
    with wave.open(path, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(16000)
        data = array("h", samples)
        wav.writeframes(data.tobytes())


def pcm(name):
    rate, samples = read_wav(speak(name, name))
    return resample(samples, rate)


build = pcm("please check the build")
over = pcm("over")
out = pcm("out")
tests = pcm("and run the tests")
look = pcm("look over this code")
meeting = pcm("the meeting is over")

fixtures = {
    "cue_end": build + silence(300) + over + silence(1000) + tests,
    "cue_submit": build + silence(300) + out + silence(1000) + tests,
    "cue_no_confirm": build + silence(300) + out + tests,
    "ordinary_over": look + silence(1000),
    "ordinary_meeting_over": meeting + silence(1000),
}

for name, samples in fixtures.items():
    path = os.path.join(ROOT, f"{name}.wav")
    write_wav(path, samples)
    print(path, round(len(samples) / 16000, 2), "s")
