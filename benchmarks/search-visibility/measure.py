#!/usr/bin/env python3
"""Time `/search` on a running site seeded by seed.sh (B248).

    python3 -I benchmarks/search-visibility/measure.py --base http://127.0.0.1:3248 \
        --label branch --requests 300 --out branch.json

Three viewers: an anonymous visitor, `gen_user_1` (holds `view own unpublished
content`) and `gen_admin`. Each sends `--requests` searches, cycling through
one-word and two-word queries over the generator's word list, after a warm-up
of 20. Prints p50 and p95 in milliseconds per viewer and writes them as JSON.
"""
import argparse
import http.cookiejar
import json
import statistics
import time
import urllib.request

WORDS = ["river", "stone", "lantern", "harbor", "meadow", "copper", "willow",
         "signal", "garden", "orbit", "canvas", "thunder", "pepper", "violet",
         "bridge", "falcon", "marble", "ember", "glacier", "orchard"]
QUERIES = WORDS + [f"{a} {b}" for a, b in zip(WORDS, WORDS[1:] + WORDS[:1])]


def opener_for(base, user, password):
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    if user:
        body = json.dumps({"username": user, "password": password}).encode()
        req = urllib.request.Request(f"{base}/user/login/json", data=body,
                                     headers={"content-type": "application/json",
                                              "x-forwarded-for": f"10.248.0.{len(user)}"})
        with opener.open(req) as r:
            assert r.status == 200, f"login {user}: {r.status}"
    return opener


def run(opener, base, n):
    times = []
    for i in range(n + 20):
        q = urllib.request.quote(QUERIES[i % len(QUERIES)])
        page = 1 + (i // len(QUERIES)) % 3
        start = time.perf_counter()
        with opener.open(f"{base}/search?q={q}&page={page}") as r:
            r.read()
            assert r.status == 200
        if i >= 20:
            times.append((time.perf_counter() - start) * 1000)
    times.sort()
    return {"n": len(times), "p50": statistics.median(times),
            "p95": times[int(len(times) * 0.95) - 1]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--requests", type=int, default=300)
    ap.add_argument("--password", default="bench-password-1234")
    ap.add_argument("--out")
    a = ap.parse_args()
    out = {"label": a.label}
    for name, user in [("anonymous", None), ("author", "gen_user_1"), ("admin", "gen_admin")]:
        out[name] = run(opener_for(a.base, user, a.password), a.base, a.requests)
        r = out[name]
        print(f"{a.label:8} {name:10} n={r['n']} p50={r['p50']:.1f}ms p95={r['p95']:.1f}ms")
    if a.out:
        with open(a.out, "w") as f:
            json.dump(out, f, indent=2)


if __name__ == "__main__":
    main()
