# 🎞️ mcpr editor

Replay Mod の保存ファイル .mcpr を編集する

`mcpr-cli --help`
```
mcpr editor cli

Usage: mcpr-cli [OPTIONS]

Options:
  -i, --input <INPUT>
  -o, --output <OUTPUT>
      --exclude-packets <EXCLUDE_PACKETS>
      --include-packets <INCLUDE_PACKETS>
  -p, --packet-details
      --unknow-packet
  -c, --compression-level <COMPRESSION_LEVEL>
      --interval <INTERVAL>                    [default: 0]
      --speed <SPEED>                          [default: 1]
  -h, --help                                   Print help
  -V, --version   
```

## Features

- [x] Library
- [x] CLI
- [x] Web App
- [ ] ...

### Library

- [x] mcpr IO
- [x] flashback IO
- [x] unzipped directory IO
- [x] packet stream
- [x] encoder / decoder

### CLI

- [x] connect
- [ ] cut
- [x] change speed
- [x] packet restriction (include/exclude)
- [x] compress
- [x] show packet details
