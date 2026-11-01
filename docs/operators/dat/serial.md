# Serial (`dat.serial`)

DAT · version 1 · cook budget 40 µs

Reads lines from a serial port (8 data bits, no parity, one stop bit) on a worker thread: each line received becomes a row of its output table (a `line` header row, then the newest `Keep` lines, newest last). Each new value on the `send` input is written to the port, followed by the line ending. Reopens the port every second while Active and it is missing or unplugged.

Cooks every frame (time-dependent).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `send` | Dat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `port` | Port | string | `""` |  | The port: `/dev/tty.usbmodem1101` or `/dev/cu.usbserial-…` on macOS, `/dev/ttyUSB0` on Linux, `COM3` on Windows. Empty: closed. |
| `baud` | Baud | int | `9600` | 300 – 4000000 | Bits per second; it must match the device (9600 and 115200 are common). |
| `line_end` | Line End | menu | `"lf"` | `lf`, `crlf`, `cr`, `none` | Written after each value sent. Lines received may end with any of them. |
| `active` | Active | toggle | `true` |  | Keeps the port open while on; off closes it. |
| `keep` | Keep | int | `20` | 1 – 1000 | Lines kept in the output table. |

## Platforms

All platforms.

## Examples

- [DAT serial](../../../examples/dat-serial.lasair/) — `board`
