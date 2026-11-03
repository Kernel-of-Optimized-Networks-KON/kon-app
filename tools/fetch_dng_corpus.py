"""Fetch the real DNG test corpus pinned in tests/fixtures/dng-corpus.tsv (name, sha256, bytes, url).

Usage: python tools/fetch_dng_corpus.py <dest_dir>
Files already present with the right sha256 are kept; anything else is (re)downloaded and must match the pinned
sha256 exactly, otherwise this exits non-zero. No file is ever skipped silently.
"""
import hashlib, os, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "..", "tests", "fixtures", "dng-corpus.tsv")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url, dest):
    req = urllib.request.Request(url, headers={"User-Agent": "kon-app-ci/1.0"})
    with urllib.request.urlopen(req, timeout=120) as r, open(dest, "wb") as o:
        while chunk := r.read(1 << 20):
            o.write(chunk)


def main(dest_dir):
    os.makedirs(dest_dir, exist_ok=True)
    rows = [l.rstrip("\n").split("\t") for l in open(MANIFEST, encoding="utf-8") if l.strip() and not l.startswith("#")]
    for name, digest, size, url in rows:
        path = os.path.join(dest_dir, name)
        if os.path.exists(path) and os.path.getsize(path) == int(size) and sha256(path) == digest:
            continue
        for attempt in range(1, 6):
            try:
                download(url, path)
                got = sha256(path)
                if got != digest:
                    raise RuntimeError(f"sha256 mismatch for {name}: expected {digest}, got {got}")
                print("fetched", name, flush=True)
                break
            except Exception as e:  # network errors and hash mismatches are retried, then fatal
                print(f"attempt {attempt} failed for {name}: {e}", file=sys.stderr, flush=True)
                if attempt == 5:
                    sys.exit(f"could not fetch {name} from {url}")
                time.sleep(5 * attempt)
    print(f"corpus ok: {len(rows)} files in {dest_dir}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
