# Bundled Wine runtime

This app includes the standalone Wine runtime r17 published by WoWSilicon.
The build checks the archive against its published SHA-256 before bundling it.
The manifest at `Wine/share/wowsilicon/runtime-lock.json` records the Wine
source revision and the hashes of the overlays and support libraries. The
archive, lock file, and build scripts are available upstream.

- [Wine runtime r17 archive](https://github.com/WoWSilicon/WoWSilicon/releases/tag/wine-runtime-r17)
- [Runtime build instructions and patches](https://github.com/WoWSilicon/WoWSilicon/tree/v3.2.2/tools/wine-runtime)
- [Runtime source lock](https://github.com/WoWSilicon/WoWSilicon/blob/v3.2.2/Packaging/WineRuntime/runtime-lock.json)

| Component | Source | Included license |
| --- | --- | --- |
| Wine 11.13 | [WineAndAqua source at the runtime revision](https://github.com/WineAndAqua/wine/tree/37540b5d94ac1c86e2599ef55d7f3a15e3237ce8) | [Wine notice](licenses/Wine-LICENSE.txt), [LGPL 2.1](licenses/Wine-COPYING.LIB.txt) |
| MoltenVK 1.4.1 | [MoltenVK source](https://github.com/KhronosGroup/MoltenVK/tree/v1.4.1) | [Apache 2.0](licenses/MoltenVK-LICENSE.txt) |
| MTLd3D 0.7.0 | [MTLd3D source](https://github.com/athei/mtld3d/tree/v0.7.0) | [zlib](licenses/MTLd3D-LICENSE.txt) |
| Vulkan Loader | [Vulkan Loader source](https://github.com/KhronosGroup/Vulkan-Loader) | [Vulkan Loader license](licenses/Vulkan-Loader-LICENSE.txt) |

The archive also includes the [Mesa KosmicKrisp driver](https://docs.mesa3d.org/drivers/kosmickrisp.html)
and support libraries from [SDL](https://github.com/libsdl-org/SDL),
[FreeType](https://freetype.org/), [GMP](https://gmplib.org/),
[GnuTLS](https://www.gnutls.org/), [Nettle](https://www.lysator.liu.se/~nisse/nettle/),
[GNU libidn2](https://www.gnu.org/software/libidn/#libidn2),
[GNU gettext](https://www.gnu.org/software/gettext/), [p11-kit](https://p11-glue.github.io/p11-glue/p11-kit.html),
[libpng](http://www.libpng.org/pub/png/libpng.html), [GNU libtasn1](https://www.gnu.org/software/libtasn1/),
and [GNU libunistring](https://www.gnu.org/software/libunistring/).
