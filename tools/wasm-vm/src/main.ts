import { generateDeviceTree } from "./devicetree.ts";

const cmdline = "no_hash_pointers";
const PAGE_SIZE = 1 << 16; // 64KiB
const memoryPages = 1024;

const memory: WebAssembly.Memory = new WebAssembly.Memory({
  initial: memoryPages,
  maximum: memoryPages,
  shared: true,
});
const vmlinux = await WebAssembly.compileStreaming(fetch("/vmlinux.wasm"));

function listen(worker: Worker) {
  worker.onmessage = (ev) => {
    switch (ev.data.type) {
      case "task":
        listen(
          new Worker(new URL("./worker.ts", import.meta.url), {
            /* @vite-ignore */
            name: ev.data.name,
            type: "module",
          }),
        ).postMessage({ type: "task", vmlinux, memory, task: ev.data.task });
        break;
      case "secondary":
        listen(
          new Worker(new URL("./worker.ts", import.meta.url), {
            name: "secondary",
            type: "module",
          }),
        ).postMessage({
          type: "secondary",
          vmlinux,
          memory,
          cpu: ev.data.cpu,
          idle: ev.data.idle,
        });
        break;
    }
  };
  return worker;
}

const devicetree = generateDeviceTree({
  chosen: {
    "rng-seed": crypto.getRandomValues(new Uint8Array(64)),
    bootargs: cmdline,
  },
  aliases: {},
  memory: {
    device_type: "memory",
    reg: [0, memoryPages * PAGE_SIZE],
  },
});
listen(
  new Worker(new URL("./worker.ts", import.meta.url), {
    name: "boot",
    type: "module",
  }),
).postMessage({ type: "boot", vmlinux, memory, devicetree }, [
  devicetree.buffer,
]);
