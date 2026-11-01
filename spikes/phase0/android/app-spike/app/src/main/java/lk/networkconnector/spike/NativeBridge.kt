package lk.networkconnector.spike

/** JNI functions exported by libnc_wasm_spike.so (spikes/phase0/android/wasm-spike). */
object NativeBridge {
    const val STATUS_STOPPED = Long.MIN_VALUE
    const val STATUS_ERROR = Long.MIN_VALUE + 1

    init {
        System.loadLibrary("nc_wasm_spike")
    }

    /** Sum of 0 until n computed inside Wasm, or a STATUS_* value. Blocks; call off the main thread. */
    @JvmStatic external fun nativeRun(n: Long): Long

    /** Ask a running nativeRun to stop. Any thread. */
    @JvmStatic external fun nativeStop()

    /** 1 = wasmtime-pulley, 2 = wasmtime-cranelift, 3 = wasmi. */
    @JvmStatic external fun nativeRuntime(): Int

    fun runtimeName(): String = when (nativeRuntime()) {
        1 -> "wasmtime-pulley"
        2 -> "wasmtime-cranelift"
        3 -> "wasmi"
        else -> "unknown"
    }
}
