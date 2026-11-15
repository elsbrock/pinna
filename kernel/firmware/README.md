# Firmware built into the kernel

`i2s_to_spi_v34.bin` is the bitstream for the Echo's microphone FPGA, which
bridges the microphones' I2S to SPI. The SPI capture driver loads it at probe,
before the root is mounted, so it is built into the kernel
(`CONFIG_EXTRA_FIRMWARE`). It is Amazon's file, taken unmodified from
the previous firmware's Linux tree (`firmware/i2s_to_spi_v34.bin`, added in 393b8a1e1);
there is no other public source.

sha256 `77a558bacdaaf9e343f02f2d74f27a5f2bb2dc8b6d66cc2499b60ed14ef62fe6`
