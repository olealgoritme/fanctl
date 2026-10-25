# fanctl

See and control every fan, pump and temperature sensor on Linux.

- Motherboard fans, AMD GPUs, and USB coolers with a kernel driver (NZXT, Corsair, Aquacomputer, ...)
- Lian Li Uni fan hubs over USB
- NVIDIA GPU fans

## Install

```
cargo build --release
sudo install -m 755 target/release/fanctl /usr/local/bin/
```

## Use

```
fanctl                       live view (sudo fanctl to change fans)
fanctl probe                 list everything found
fanctl set lianli/* 70       set fans to 70 %
fanctl identify lianli/fan2  find out which fan that is
```

## Fan curves at boot

```
sudo cp fanctl.toml.example /etc/fanctl.toml      # edit the ids from `fanctl probe`
sudo cp fanctl.service /etc/systemd/system/
sudo systemctl enable --now fanctl
```

If a sensor can't be read, or the service stops, all its fans go to 100 %.

## Credits

Made by Ole Algoritme. MIT license.

Lian Li protocol from [liquidctl](https://github.com/liquidctl/liquidctl) and
[uni-sync](https://github.com/EightB1ts/uni-sync).
