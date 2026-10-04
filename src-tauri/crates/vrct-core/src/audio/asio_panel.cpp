// The ASIO SDK implements this function, but asio-sys does not bind it.
// Windows ASIOError is a signed 32-bit long; retain the SDK's C++ linkage.
long ASIOControlPanel();
extern "C" long vrct_asio_control_panel() { return ASIOControlPanel(); }
