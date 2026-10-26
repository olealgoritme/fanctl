# fanctl

See and control every fan, pump and temperature sensor on Linux.

- Motherboard fans, AMD GPUs, and USB coolers with a kernel driver (NZXT, Corsair, Aquacomputer, ...)
- Lian Li Uni fan hubs over USB
- NVIDIA GPU fans, and NVIDIA VRAM temperature (RTX 3000/4000 GDDR6/6X, RTX 5090 GDDR7 per module; root)

## Install

```
cargo build --release
sudo install -m 755 target/release/fanctl /usr/local/bin/
```

## Use

In the live view, ←→ (or 1-9, f) change the selected fan right away. While the service runs,
that fan keeps your value, also after a reboot, until you press `a`; the other fans stay on
their curves. Members of the sudo/wheel group can do this without sudo.

```
fanctl                       live view
fanctl probe                 list everything found
fanctl set lianli/* 70       set fans to 70 % (kept until auto)
fanctl auto lianli/*         back to the curve
fanctl identify lianli/fan2  find out which fan that is
```

## Fan curves at boot

```
sudo cp fanctl.toml.example /etc/fanctl.toml      # edit the ids from `fanctl probe`
sudo cp fanctl.service /etc/systemd/system/
sudo systemctl enable --now fanctl
```

If a sensor can't be read, or the service stops, all its fans go to 100 %. A cooler whose
driver loads after the service is picked up on its own.

## Credits

Made by Ole Algoritme. MIT license.

Lian Li protocol from [liquidctl](https://github.com/liquidctl/liquidctl) and
[uni-sync](https://github.com/EightB1ts/uni-sync). VRAM registers from
[gddr6](https://github.com/olealgoritme/gddr6) and [gpuwatch](https://github.com/olealgoritme/gpuwatch).
