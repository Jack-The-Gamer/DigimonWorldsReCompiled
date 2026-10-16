# Built-in BIOS: OpenBIOS

`openbios.bin` is [OpenBIOS](https://github.com/pcsx-redux/nugget/tree/main/openbios),
the open-source PlayStation BIOS written by the PCSX-Redux authors, used when a
player has not added their own console BIOS. It is MIT-licensed (see
`LICENSE` in this folder) and contains no Sony code.

It is built without its menu shell, so it boots the disc directly:

```sh
git clone https://github.com/pcsx-redux/nugget && cd nugget
git checkout 58d9430b357511635ba874c07e08cbbde1ca3c7f
cd openbios
make PREFIX=mipsel-linux-gnu FORMAT=elf32-tradlittlemips FASTBOOT=1
```

(`mipsel-linux-gnu-gcc` 12.4 from Ubuntu 24.04's `gcc-mipsel-linux-gnu`;
the upstream `mipsel-none-elf` toolchain works with `PREFIX=mipsel-none-elf`
and the default `FORMAT`.)

Tested with Re:Compiled: Digimon World (USA), Digimon World 2 (USA) and
Digimon World 2003 (Europe) boot, play and save. A memory card written through
OpenBIOS is byte-for-byte the same as one written through a Sony BIOS
(SCPH-5501), so saves move freely between the two. Game code runs exactly as
with a Sony BIOS; only the BIOS's own routines (kernel calls, boot sequence)
differ in timing, which is why your own BIOS is used whenever you add one.
