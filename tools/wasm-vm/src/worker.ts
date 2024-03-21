import { assert } from "./util.ts";

const textDecoder = new TextDecoder();

function instantiate(
  vmlinux: WebAssembly.Module,
  wasmMemory: WebAssembly.Memory,
  get_dt = (_buf: number, _size: number) => {
    console.error("get_dt called on non-boot thread");
  },
) {
  const memory = new Uint8Array(wasmMemory.buffer);

  let irqflags = 0;
  const imports = {
    env: { memory: wasmMemory },
    kernel: {
      breakpoint() {
        debugger;
      },
      halt() {
        self.close();
      },
      restart() {
        globalThis?.location?.reload?.();
      },
      boot_console_write(msg: number, len: number) {
        const message = memory.slice(msg, msg + len);
        const textMessage = textDecoder.decode(message);
        console.log(textMessage);
      },
      boot_console_close() {},
      return_address() {
        return 0;
      },
      // return_address(n: number) {
      //   const matches = new Error().stack?.matchAll(BACKTRACE_ADDRESS_RE);
      //   if (!matches) return -1;
      //   const match = iteratorNth(matches, n + 1);
      //   return parseInt(match?.[1] ?? "-1");
      // },
      set_irq_enabled(flags: number) {
        irqflags = flags;
      },
      get_irq_enabled() {
        return irqflags;
      },
      get_dt,
      get_now_nsec() {
        /*
                The more straightforward way to do this is
                `BigInt(Math.round(performance.now() * 1_000_000))`.
                Below is semantically identical but has less floating point
                inaccuracy.
                `performance.now()` has 5μs precision in the browser.
                In server runtimes it has full nanosecond precision, but this code
                rounds to the same 5μs precision.
                */
        return (
          BigInt(
            Math.round((performance.now() + performance.timeOrigin) * 200),
          ) * 5000n
        );
      },
      get_stacktrace(buf: number, size: number) {
        // 5 lines: strip Error, strip 4 common lines of stack
        const trace = new TextEncoder().encode(
          new Error().stack?.split("\n").slice(5).join("\n"),
        );
        if (size >= trace.byteLength) {
          /// 46 = "."
          trace[size - 1] = 46;
          trace[size - 2] = 46;
          trace[size - 3] = 46;
        }
        memory.set(trace.slice(0, size), buf);
      },
      new_worker(task: number, comm: number, commLen: number) {
        const name = new TextDecoder().decode(
          memory.slice(comm, comm + commLen),
        );
        self.postMessage({ type: "task", task, name });
      },
      bringup_secondary(cpu: number, idle: number) {
        self.postMessage({ type: "secondary", cpu, idle });
      },
    },
  } satisfies WebAssembly.Imports;

  return new WebAssembly.Instance(vmlinux, imports);
}

self.onmessage = (ev) => {
  switch (ev.data.type) {
    case "boot":
      (
        instantiate(
          ev.data.vmlinux,
          ev.data.memory,
          (buf: number, size: number) => {
            assert(
              size >= ev.data.devicetree.byteLength,
              "Device tree truncated",
            );
            new Uint8Array(ev.data.memory.buffer).set(
              ev.data.devicetree.slice(0, size),
              buf,
            );
          },
        ).exports["boot"] as CallableFunction
      )();
      break;
    case "task":
      (
        instantiate(ev.data.vmlinux, ev.data.memory).exports[
          "task"
        ] as CallableFunction
      )(ev.data.task);
      break;
    case "secondary":
      (
        instantiate(ev.data.vmlinux, ev.data.memory).exports[
          "secondary"
        ] as CallableFunction
      )(ev.data.cpu, ev.data.idle);
      break;
  }
};

export {};
