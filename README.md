# ffi-playground

Exploring hooking into macOS's internal libraries with Rust and `libloading`.

## Information

This project is composed of two parts:

- MacBook Fan Control (SMC) with `sudo ffi-playground fancontrol`,
- and MacBook Touchpad experimentation with `sudo ffi-playground --help`.

Both hook into the `PrivateLibraries` of macOS. The SMC functionality offers a real-time TUI (terminal user interface) utilizing the `ratatui` library including full control over individual fans in a MacBook, along with a real-time graph of fan speed and processor temperature.

The MacBook touchpad also includes a custom TUI that shows all information available from the touchpad, including an ASCII powered viewer where you can see exactly what inputs the touchpad is detecting in real time.

## License

Licensed under the EUPL v1.2. See [LICENSE](./LICENSE).
