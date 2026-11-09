# Memory guard: manual stress test

This is the only test that puts real pressure on the machine. Automated tests use a fake
sampler and never allocate on purpose.

**Before you start: save your work and close the apps you care about** (browsers, IDEs,
other agents). Pushing a Mac into critical memory pressure can make it sluggish for a
minute and macOS may kill background apps. Stop at any moment with Ctrl-C in the hog's
terminal; the memory is released at once.

## The hog

`hog.py` allocates memory in steps and touches every page (untouched pages cost nothing):

```python
# hog.py: allocate STEP_MB every 5 s up to MAX_MB, touching every page; Ctrl-C frees all.
import sys, time
max_mb = int(sys.argv[1]) if len(sys.argv) > 1 else 1024
step_mb = int(sys.argv[2]) if len(sys.argv) > 2 else 256
held = []
while sum(len(b) for b in held) < max_mb << 20:
    b = bytearray(step_mb << 20)
    for i in range(0, len(b), 4096):
        b[i] = 1
    held.append(b)
    print(f"holding {sum(len(b) for b in held) >> 20} MB", flush=True)
    time.sleep(5)
print("at the cap; Ctrl-C to free", flush=True)
time.sleep(3600)
```

Save it inside a task's worktree (so it runs in that session's process tree).

## A. Per-session budget (safe: stays below a quarter of RAM)

1. `mirante new <repo> --agent claude --branch mem-a "wait for instructions"`; keep
   `mirante watch` open in another terminal (its header shows the system pressure level).
2. In `mirante attach mem-a`, ask Claude to run `python3 hog.py 3500 256` and allow it once.
3. Expect, as it grows: `mirante ls` MEM column climbing; `memory.warn` with `soft` at 80 %
   of the budget (default min(4 GiB, 25 % of RAM)), then `hard` at 100 %. Nothing is killed:
   the session is working (a tool is running).
4. Ctrl-C the hog (Esc in attach, or `mirante interrupt mem-a`). MEM drops.

## B. System pressure (only with apps closed; stop as soon as it reacts)

1. Two idle sessions (`mem-a`, `mem-b`) plus a hog run from a plain terminal outside Mirante:
   `python3 hog.py <about 80 % of RAM in MB> 512`.
2. Expect: the `mirante watch` header goes `elevated` (the kernel pressure level reaches 2 — `sysctl kern.memorystatus_vm_pressure_level`; swap use alone does not count)
   with `pausing: off`, and an offer to hibernate the largest idle session (`mirante
   hibernate <id>` to accept). **Nothing pauses by default**: new dispatches and queued
   deliveries go on (a busy Mac reports `warn` most of the time). To check the old
   behaviour, set **Pause new sessions under memory pressure** to *When elevated* in the
   app (Sessions), or start the daemon with `MIRANTE_MEMGUARD_PAUSE=elevated`: then expect
   `memory.paused` here and `memory.resumed` once it eases (`critical`: only from step 3).
3. Ctrl-C the hog the moment the header shows `critical`. Only if you deliberately let
   critical last 10 s: the largest IDLE session without a pending permission is hibernated
   (`memory.auto_hibernated`), at most one per minute; `mirante wake <id>` resumes it. This
   happens whatever the pause setting says (it is the guard against a frozen machine).
4. With a permission dialog open in `mem-b`, `mem-b` is never hibernated automatically; the
   event `memory.refused` says so.

## Record

| date | scenario | peak MB | events seen | hibernated (auto / offered) | ok? |
|---|---|---|---|---|---|
| | | | | | |
