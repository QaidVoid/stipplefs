
## [0.1.1](https://github.com/QaidVoid/stipplefs/compare/v0.1.0...v0.1.1) - 2026-10-21

### ⚙️ Miscellaneous Tasks

- Attach binaries to v-prefixed releases - ([627fac6](https://github.com/QaidVoid/stipplefs/commit/627fac6fbe46d04e7fd839319ee79f5c20551320))

## [0.1.0] - 2026-10-21

### ⛰️  Features

- *(format)* Record how much of the stream a hot list fills - ([41ef537](https://github.com/QaidVoid/stipplefs/commit/41ef53733f6640616ad1e98cf584d948644c34bb))
- *(format)* Add BCJ codecs and compressed metadata - ([511c6f8](https://github.com/QaidVoid/stipplefs/commit/511c6f8ddbe16daadacbff2be2a6ba3738c54d28))
- *(mkstipple)* Default to 32 MiB blocks, 4 KiB chunks, ext order - ([fd60179](https://github.com/QaidVoid/stipplefs/commit/fd60179fb3f3be2d2641f9963272cf63ad9e7c49))
- *(mkstipple)* Report progress while building - ([984216a](https://github.com/QaidVoid/stipplefs/commit/984216a582912cb01537d85b3e036f6a5c08bcfb))
- *(mount)* Warm by default, starting with the hot region - ([8b36a33](https://github.com/QaidVoid/stipplefs/commit/8b36a335fd0482fc83a96ac92d0303994574a39f))
- *(mount)* Warm blocks into the kernel page cache - ([2a2990f](https://github.com/QaidVoid/stipplefs/commit/2a2990fccd3025f2f285047f66be2ae499f47c92))
- *(write)* Add Builder::insert_file_at - ([03e9b35](https://github.com/QaidVoid/stipplefs/commit/03e9b35e3f9e6244ab749417e31b5d45a1b0cf63))
- *(write)* Place a hot list of files first in the stream - ([73cf81d](https://github.com/QaidVoid/stipplefs/commit/73cf81dddffdf11442032386ceb08f0bad9c5944))
- Add --order ext and latency percentiles to bench - ([a8902fa](https://github.com/QaidVoid/stipplefs/commit/a8902fa4134fb7a110ede33325a3e3d52d57fef8))
- Refuse a compression level outside the usable range - ([19f1506](https://github.com/QaidVoid/stipplefs/commit/19f15062fff0b0fc7fdc13549cc83c397fb271dc))
- Present every kind, time and attribute through the mount - ([cdc3802](https://github.com/QaidVoid/stipplefs/commit/cdc38021fa7b11181275c6c83e1e32448184010f))
- Report entry kinds and refuse bad table indexes - ([f4245bb](https://github.com/QaidVoid/stipplefs/commit/f4245bbb215c582b4f0f1db30d3b0f717b2be904))
- Keep writer memory off the size of the tree - ([d05a45a](https://github.com/QaidVoid/stipplefs/commit/d05a45a9d465292735c801bd184282be04099699))
- Record every entry kind, attributes and times - ([893e10c](https://github.com/QaidVoid/stipplefs/commit/893e10c98cac973b53e12b432faea37e88ecd334))
- Lift the block size ceiling to 2 GiB - ([d4f85e4](https://github.com/QaidVoid/stipplefs/commit/d4f85e40cc107ec1af237dcab66b702a10451399))
- Add path resolution and directory iteration - ([4fe7454](https://github.com/QaidVoid/stipplefs/commit/4fe745496005ef9d596eb9ab50bb6f9673eb2255))
- Mount images through FUSE - ([483604e](https://github.com/QaidVoid/stipplefs/commit/483604e161c9fce1853faa73c6b94cfbb8536c64))
- Add mkstipple and stipple command line tools - ([4ef7424](https://github.com/QaidVoid/stipplefs/commit/4ef7424a389099a3df43b8393e909be5654437c9))
- Train dictionaries and verify against a reference - ([6d3b197](https://github.com/QaidVoid/stipplefs/commit/6d3b197c1d06d8f3ba0d18e0e623e49f9a4ff867))
- Add a deterministic image writer - ([45337a2](https://github.com/QaidVoid/stipplefs/commit/45337a299c400c14586ed8fd30537a12e82b41b0))
- Support dictionary blocks and verify integrity - ([ac54974](https://github.com/QaidVoid/stipplefs/commit/ac549743fe23df31388cf7999fb02b6f223ca47f))
- Add zstd as an opt-in block codec - ([1b507f1](https://github.com/QaidVoid/stipplefs/commit/1b507f1ef293fbe8b14784ccd8e6557c0b950cb3))
- Add a single-flight bounded block cache - ([4c543dc](https://github.com/QaidVoid/stipplefs/commit/4c543dc8f54e4caf592aa971ccd19acb562ba960))
- Resolve extents and serve reads from an image - ([858d204](https://github.com/QaidVoid/stipplefs/commit/858d204c1f54cdaa3a34ac11e7b30b1e1a1a45ed))
- Add mmap backend and untrusted-input suite - ([9178e78](https://github.com/QaidVoid/stipplefs/commit/9178e78bc87bcff2d565951ceab8a9cee2164de2))
- Read metadata sections and the block index - ([cb37e55](https://github.com/QaidVoid/stipplefs/commit/cb37e554c6c0e6a4a51be96dd923b1468d24947f))
- Open and validate images from a byte range - ([7df0275](https://github.com/QaidVoid/stipplefs/commit/7df0275218e6d832abdd66ef5fd0a6758941f96f))
- Add byte source abstraction and image region - ([4325201](https://github.com/QaidVoid/stipplefs/commit/4325201d8afa27f7aa4cd64c5765cda347839c5b))

### 🐛 Bug Fixes

- *(stipple)* End quietly when a closed pipe cuts output short - ([7c9f622](https://github.com/QaidVoid/stipplefs/commit/7c9f62254c281c15e54142fca7359d7f5339d488))
- *(stipple)* Print bench counters only when they are collected - ([7af0699](https://github.com/QaidVoid/stipplefs/commit/7af0699070859e3b32d425eb5df417df0c7354e8))
- *(stipple)* Restore modes, links and names on extract - ([448f1af](https://github.com/QaidVoid/stipplefs/commit/448f1af36e0bfa9e7db682a1894b54408dfb9e0d))
- *(stipple)* Unmount cleanly when stopped by a signal - ([1cc0d5a](https://github.com/QaidVoid/stipplefs/commit/1cc0d5ac5964a1da124b2649c52358bb182e2470))
- *(write)* Split scratch files to stay under file size limits - ([59742ae](https://github.com/QaidVoid/stipplefs/commit/59742ae4947a8cc3a7e8a7fd6357f9df2bcbc878))

### 🚜 Refactor

- *(write)* Set the file order on Builder, not in Options - ([f908ea0](https://github.com/QaidVoid/stipplefs/commit/f908ea0a20c9dcbd349b9618aca06260f5cfef6d))
- Tidy the extension sort and the bench command - ([395f49f](https://github.com/QaidVoid/stipplefs/commit/395f49f1e4765e182596a037eafd943edbae99db))
- Place file content in a pass of its own - ([4acd580](https://github.com/QaidVoid/stipplefs/commit/4acd580e37b9cc2577865c48283ff85de28b992c))

### 📚 Documentation

- Add a README and a VitePress documentation site - ([5cd1828](https://github.com/QaidVoid/stipplefs/commit/5cd1828bf446ec0579203249b4e3454b316eabe5))
- Specify the on-disk image format - ([de254fe](https://github.com/QaidVoid/stipplefs/commit/de254fe4c7ce5b523f01e9f81fd6074ed0485039))

### ⚡ Performance

- *(bcj)* Skip bytes that cannot start a call eight at a time - ([e18f107](https://github.com/QaidVoid/stipplefs/commit/e18f1077ba97d8447c5c7a56aefd598ac60086aa))
- *(mkstipple)* Use every core and drop dictionaries on big blocks - ([bbf1e37](https://github.com/QaidVoid/stipplefs/commit/bbf1e373a28ba5282538280c14471c04def26033))
- *(mount)* Warm with up to twelve workers - ([2be2ed5](https://github.com/QaidVoid/stipplefs/commit/2be2ed5dc4024131ba56968d7e81764d5adfb4a4))
- *(write)* Match zstd's window to the block - ([d63480e](https://github.com/QaidVoid/stipplefs/commit/d63480eaf50cd61c6dcc49210cbea02f55530eaf))
- Cap what a sequential run keeps in the cache - ([8ad046a](https://github.com/QaidVoid/stipplefs/commit/8ad046a5734de44c2bad4ee5909545cb0d4e46e8))
- Keep streamed blocks from displacing reused ones - ([65fd605](https://github.com/QaidVoid/stipplefs/commit/65fd605b0eab23dc9f441d2f8bd3633c807a66ac))
- Choose evictions without scanning the cache - ([f5efdf0](https://github.com/QaidVoid/stipplefs/commit/f5efdf079e3a3ac925ca2989b8b1dba18203a58e))
- Stop copying every decoded block into the cache - ([0d3b32d](https://github.com/QaidVoid/stipplefs/commit/0d3b32dd95f2ff2fc44340ee71eb6fa836fde893))
- Read blocks and digests in one call, size decodes - ([8641270](https://github.com/QaidVoid/stipplefs/commit/86412700b69f2794084bd9049f402a2d11edcd2e))
- Promote cache entries in constant time - ([66a5805](https://github.com/QaidVoid/stipplefs/commit/66a5805657ce9c1a91b81fd60d92d5d379ff7f63))

### 🧪 Testing

- Cover both timestamp policies and memory limits - ([871512b](https://github.com/QaidVoid/stipplefs/commit/871512b6b382033139a2ab0cb5bf328225c2bd84))

### ⚙️ Miscellaneous Tasks

- Release to crates.io with release-plz - ([e95b88a](https://github.com/QaidVoid/stipplefs/commit/e95b88a2fbd87d6f01176428b4bf9e196b4cadec))
- Derive feature matrix from the manifest - ([616d853](https://github.com/QaidVoid/stipplefs/commit/616d853fd1d4d935fed835156380f27b3a451785))
- Add twox-hash for xxh3 integrity digests - ([72ee807](https://github.com/QaidVoid/stipplefs/commit/72ee80714362036b54f3bb99df43d25e0b559572))
- Scaffold crate and feature-combination matrix - ([3fbb785](https://github.com/QaidVoid/stipplefs/commit/3fbb7853b4ea7f7a61aa2a946f8e0e17c4621608))
