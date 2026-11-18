# Changelog

## 0.2.0

- `niruio heal [--login]`: when WirePlumber probed a sound card before the hardware was ready, the card offers its output and its input only as separate profiles and the microphone (or the speakers) is gone until the next restart of the sound service. `heal` finds such a card, restarts only WirePlumber so it probes the card again, checks the result and logs it in `~/.local/state/niruio/heal.log`. NIRUWM runs `niruio heal --login` once after each login; it does nothing when every card looks right. Seen on an HP ZBook whose speaker amplifiers load firmware at start.
- `niruio heal --force` probes every card again even when all looks right.
- `niruio doctor` reports such a card as a warning ("the microphone is missing") with the fix "probe again", and no longer calls it a working fallback.
- Restarting the sound from the doctor also works where the session starts PipeWire (Void): only WirePlumber is restarted, and the session stops it at logout.

## 0.1.1

- **Fallback card profiles:** one finding per card instead of one per output
  and input. A fallback profile whose outputs and inputs all work (many
  laptops with speaker amplifiers, such as the HP ZBook 8 G1a) is information,
  not a warning. The doctor never offers Pro Audio or a profile that loses an
  output or input; it offers only an available profile that keeps every
  direction, with available ports.

## 0.1.0

- **First release.** Outputs, inputs and programs with live meters, volume,
  mute, balance, default device, ports and profiles; moving a program to
  another device.
- **Microphones:** ALSA boost, a five second mic test with peak, RMS and
  clipping, per-input meters of multichannel interfaces, and the inputs
  NIRUCLIP records.
- **Cameras:** users, formats, V4L2 controls, NIRUCLIP's default camera and a
  live preview (kitty graphics, half blocks elsewhere).
- **Cards and Bluetooth:** profiles and ports, hi-fi or headset mode, codec.
- **Saved and Doctor:** what WirePlumber remembers per role and program, safe
  resets through WirePlumber itself, and a doctor with one fix per finding.
- **Command line:** `status`, `doctor` (`--json`, `--brief`, `--fix`), `saved`
  and `reset`, for `nirux doctor` and the launcher.
