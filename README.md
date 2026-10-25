# fanctl

Find and control every fan, pump and temperature sensor on a Linux machine, from one small
binary: probe, fixed duties, and a curve daemon that starts at boot.

| Source | What it covers |
|---|---|
| hwmon (`/sys/class/hwmon`) | motherboard Super I/O chips (nct67xx, it87, ...), CPU and AMD GPU sensors, NVMe and RAM sensors, and every USB cooler or hub with a kernel driver: NZXT Kraken X/Z/2023 and Smart Device v2, Corsair Commander Pro and PSUs, Aquacomputer D5 Next/Octo/Quadro, Gigabyte Waterforce, ASUS ROG Ryujin, ... |
| USB (hidraw) | Lian Li Uni hubs (SL, AL, SL v2, AL v2, SL-Infinity), which have no kernel driver |
| NVML | NVIDIA GPUs: temperature and fan duty (setting a fan needs root) |

`fanctl probe` also lists USB devices from cooling vendors that none of the above reaches.

## Use

`fanctl` on its own opens the live view: temperatures, every connected fan and pump, and keys
to set a duty, hand a fan back to its hardware, find a fan, and pause or start the service
(`sudo fanctl` to change anything).

```
fanctl probe                 everything found
fanctl status                temperatures and connected fans
fanctl set lianli/* 70       fixed duty (an id, chip/* or all)
fanctl auto nct6799/pwm2     back to the hardware's own control
fanctl identify lianli/fan2  100 % then 30 %, to find which fan it is
fanctl run /etc/fanctl.toml  curve daemon
```

Writing needs root. Build with `cargo build --release` (Rust 1.74 or newer).

## Curves

See `fanctl.toml.example`. Each curve maps the hottest of its sensors to a duty for its
controls, ramps down slowly and up at once. `[[critical]]` entries put every curve to 100 %
while a sensor is above its limit; an unreadable sensor, and stopping the daemon, also mean
100 %. Hubs are told their duty again every 30 s, since they forget it on a USB reset.

## Install

```
cargo build --release
sudo install -m 755 target/release/fanctl /usr/local/bin/
sudo install -m 644 fanctl.toml.example /etc/fanctl.toml   # then edit the ids
sudo install -m 644 fanctl.service /etc/systemd/system/
sudo systemctl enable --now fanctl
```

## Credits

fanctl is made by Ole Algoritme.

The Lian Li Uni protocol follows liquidctl's `lianli_uni` driver and EightB1ts' uni-sync.
