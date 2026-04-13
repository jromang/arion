# Built-in rigctld server

Arion embeds a **TCP server compatible with `rigctld`** (a subset
of the Hamlib protocol). Once enabled, any application supporting
"Hamlib NET rigctl" can drive Arion: QSY, mode change, audio
volume, etc.

## What is rigctld?

[`rigctld`](https://hamlib.sourceforge.net/) is the standard daemon
of the Hamlib project: it talks to a radio over its serial link
(CAT, USB, …) and exposes **a TCP socket** on which third-party
clients (WSJT-X, fldigi, GPredict, CQRLOG, Log4OM, Ham Radio
Deluxe, …) send simple text commands (`F 14074000\n`,
`M USB 2400\n`, `f\n`, …).

Arion has no CAT link — it's a native SDR transceiver — but it
**speaks the rigctld TCP protocol directly**. From the clients'
point of view, Arion appears as a local Hamlib rig.

## Enabling it in Arion

1. Open the **Setup** window (menu or floating window).
2. **Network** tab.
3. Check *Enable rigctld server*.
4. The default port is **4532** (the canonical Hamlib port).
5. The status becomes *running on 127.0.0.1:4532*.

The setting is persisted in `arion.toml`:

```toml
[network]
rigctld_enabled = true
rigctld_port    = 4532
```

On the next Arion launch, the server restarts automatically if the
box was checked. To change port, uncheck first, adjust the port,
then re-check.

## Configuring WSJT-X

In *Settings → Radio*:

| Field            | Value                   |
|------------------|-------------------------|
| Rig              | *Hamlib NET rigctl*     |
| Network Server   | `127.0.0.1:4532`        |
| PTT Method       | *CAT* (stub) or *VOX*   |
| Poll Interval    | 1 s                     |

Click **Test CAT**: the button should turn green.

> **Note**: Arion does not yet have a transmit chain. `T 1`
> (PTT ON) commands are accepted but trigger no transmission. For
> listen-only FT8, that's already sufficient.

### fldigi

*Configure → Rig Control → Hamlib*:
- *Use Hamlib*: checked
- *Rig*: `Hamlib NET rigctl (stub)`
- *Device*: `localhost:4532`

### GPredict

*Edit → Preferences → Interfaces → Radios*: create an entry with
`Host = localhost`, `Port = 4532`, `Type = RX only`.

### CQRLOG

*File → Preferences → TRX control*: check *Use Hamlib NET rigctl*,
fill in `127.0.0.1:4532`.

## Supported commands

| Verb                  | Description                       | Status        |
|-----------------------|-----------------------------------|---------------|
| `F <hz>` / `set_freq` | Sets the active VFO               | Supported     |
| `f` / `get_freq`      | Reads the active VFO frequency    | Supported     |
| `M <mode> [bw]`       | Sets mode + bandwidth             | Supported     |
| `m` / `get_mode`      | Reads mode + bandwidth            | Supported     |
| `V <vfo>`             | Selects VFO (A=RX1, B=RX2)        | Supported     |
| `v` / `get_vfo`       | Reads the active VFO              | Supported     |
| `L AF <v>`            | Audio volume (0.0–1.0)            | Supported     |
| `l AF`                | Reads audio volume                | Supported     |
| `L <other> <v>`       | Other levels (RFPOWER, SQL…)      | RPRT -11      |
| `T <0\|1>` / `set_ptt`| PTT (stub, accepted but inert)    | stub          |
| `t` / `get_ptt`       | Always 0                          | stub          |
| `S <0\|1> <vfo>`      | TX split (stub)                   | stub          |
| `s` / `get_split_vfo` | Split off                         | stub          |
| `\chk_vfo`            | VFO probe (returns `CHKVFO 0`)    | Supported     |
| `\dump_state`         | Rig info (used by WSJT-X)         | Supported     |
| `q` / `\quit`         | Closes the connection             | Supported     |
| *other*               | -                                 | RPRT -11      |

**Recognised modes**: `LSB`, `USB`, `CW`, `CWR`, `AM`, `AMS`/`SAM`,
`FM`, `PKTLSB` (DIGL), `PKTUSB` (DIGU), `DSB`.

### Extended mode (`+`)

Clients that prefix their commands with `+` ("extended" mode)
receive `Key: Value` lines in place of raw values. This mode is
transparently supported; just send `+f`, `+m`, `+\dump_state`,
etc.

## Examples

### netcat

```sh
$ nc 127.0.0.1 4532
f
14074000
RPRT 0
F 7074000
RPRT 0
m
USB
2400
RPRT 0
q
```

### Python

```python
import socket

def send(cmd: str) -> str:
    s = socket.create_connection(("127.0.0.1", 4532))
    s.sendall((cmd + "\n").encode())
    buf = b""
    while not buf.endswith(b"RPRT 0\n") and not buf.endswith(b"RPRT -1\n"):
        buf += s.recv(4096)
    s.close()
    return buf.decode()

print(send("f"))
print(send("F 14074000"))
print(send("M USB 2400"))
```

## Troubleshooting

- **`Connection refused`**: the *Enable rigctld server* box is not
  checked, or the server failed to bind (port already in use, see
  the log with `RUST_LOG=info`).
- **Port in use**: another instance of `rigctld`, FLRig or Arion
  is already running. `ss -ltnp | grep 4532` to diagnose.
- **WSJT-X "Hamlib error"**: check that the *Network Server*
  field is exactly `127.0.0.1:4532` (no `http://`, no spaces) and
  that *Poll Interval* ≥ 1 s.
- **Firewall**: by default Arion only binds to `127.0.0.1`, so no
  local rule is needed; for network access, the code will need
  adjusting (the address is currently pinned to the loopback for
  security reasons).
- **QSY not reflected**: if the RX is locked (`lock`), the
  frequency change is ignored — same semantics as on the UI side.
  Unlock the RX first.

## Known limitations

- No TX chain: `T 1` does not trigger transmission.
- `\dump_state` returns a minimal skeleton inspired by the Hamlib
  *dummy* rig. Sufficient for WSJT-X, not for tools that make
  fine-grained use of the capability flags.
- No encryption or authentication: strictly local bind
  (`127.0.0.1`).
- A single "rig" exposed; the two RX appear as VFOA/VFOB.
