use clap::Parser;
use rand::Rng;
use std::{
    arch::asm,
    io::{stdout, Write},
    path::PathBuf,
    time::Instant,
};
use vm_fdt::FdtWriter;
use wasmtime::{
    Caller, Config, Engine, InstancePre, Linker, MemoryType, Module, SharedMemory, Store,
    WasmBacktraceDetails,
};

const PAGE_SIZE: u32 = 65536;

#[derive(Parser, Debug)]
struct Args {
    /// path to the wasm file
    module: PathBuf,

    /// kernel command line
    #[clap(short, long, default_value_t = String::from("no_hash_pointers"))]
    cmdline: String,

    /// amount of memory in pages (64KiB increments)
    #[clap(short, long, default_value_t = 1024)]
    memory: u32,

    /// enable debugging
    #[clap(short, long)]
    debug: bool,
}

#[derive(Clone)]
struct State {
    memory: SharedMemory,
    irq: i32,
    devicetree: Vec<u8>,
    time_origin: Instant,
    instance_pre: Option<InstancePre<State>>,
}

fn add_imports(linker: &mut Linker<State>, is_debug: bool) -> anyhow::Result<()> {
    linker.func_wrap("kernel", "breakpoint", move || {
        if !is_debug {
            return;
        };
        unsafe {
            #[cfg(target_arch = "x86_64")]
            asm!("int3");
        }
    })?;
    linker.func_wrap("kernel", "halt", || {
        println!("halt");
        // TODO: in the js impl this halts only the current thread
        std::process::exit(1);
    })?;
    linker.func_wrap("kernel", "restart", || {
        println!("restart");
        std::process::exit(1);
    })?;

    linker.func_wrap(
        "kernel",
        "boot_console_write",
        |mut caller: Caller<'_, State>, msg: u32, len: u32| {
            let State { memory, .. } = caller.data_mut();

            let msg = msg as usize;
            let len = len as usize;

            let slice = &memory.data()[msg..][..len];
            let slice = unsafe {
                &slice
                    .into_iter()
                    .map(|cell| {
                        *cell
                            .get()
                            .as_ref()
                            .expect("wasm memory is not a null pointer")
                    })
                    .collect::<Vec<_>>()
            };
            stdout().write_all(slice)?;
            Ok(())
        },
    )?;
    linker.func_wrap("kernel", "boot_console_close", || {
        println!("console closed");
    })?;

    linker.func_wrap(
        "kernel",
        "set_irq_enabled",
        |mut caller: Caller<'_, State>, enabled: i32| {
            caller.data_mut().irq = enabled;
        },
    )?;
    linker.func_wrap("kernel", "get_irq_enabled", |caller: Caller<'_, State>| {
        caller.data().irq
    })?;
    linker.func_wrap("kernel", "return_address", |_frames: i32| -1)?;

    linker.func_wrap(
        "kernel",
        "get_dt",
        |mut caller: Caller<'_, State>, buf: u32, len: u32| {
            let State {
                ref mut memory,
                devicetree,
                ..
            } = caller.data_mut();
            let memory = memory.data();
            let buf = buf as usize;
            let len = (len as usize).min(devicetree.len());
            for i in 0..len {
                unsafe {
                    *memory[buf + i].get() = devicetree[i];
                }
            }
        },
    )?;
    linker.func_wrap("kernel", "get_now_nsec", |caller: Caller<'_, State>| {
        let duration = Instant::now() - caller.data().time_origin;
        u64::try_from(duration.as_nanos())
            .expect("584 years would have to pass for this to overflow")
    })?;
    linker.func_wrap(
        "kernel",
        "get_stacktrace",
        |mut caller: Caller<'_, State>, buf: u32, len: u32| {
            let memory = caller.data_mut().memory.data();

            let trace = std::backtrace::Backtrace::force_capture()
                .to_string()
                .into_bytes();

            let buf = buf as usize;
            let len = (len as usize).min(trace.len());
            for i in 0..len {
                unsafe {
                    *memory[buf..][i].get() = trace[i];
                }
            }
        },
    )?;

    linker.func_wrap(
        "kernel",
        "new_worker",
        |mut caller: Caller<'_, State>, task: u32, comm: u32, comm_len: u32| {
            let memory = caller.data_mut().memory.data();
            let comm = comm as usize;
            let comm_len = comm_len as usize;
            let mut name = Vec::with_capacity(comm_len);

            for i in 0..comm_len {
                unsafe {
                    name.push(*memory[comm + i].get());
                }
            }

            let data = caller.data().clone();
            let instance_pre = data
                .instance_pre
                .clone()
                .expect("instance_pre is intialized before the first call");
            let engine = caller.engine().clone();

            std::thread::Builder::new()
                .name(String::from_utf8_lossy(&name).to_string())
                .spawn(move || {
                    let mut store = Store::new(&engine, data);

                    instance_pre
                        .instantiate(&mut store)
                        .unwrap()
                        .get_typed_func::<u32, ()>(&mut store, "task")
                        .expect("the function exists")
                        .call(&mut store, task)
                        .unwrap();
                })?;

            Ok(())
        },
    )?;
    linker.func_wrap(
        "kernel",
        "bringup_secondary",
        |caller: Caller<'_, State>, cpu: u32, idle: u32| {
            let data = caller.data().clone();
            let instance_pre = data
                .instance_pre
                .clone()
                .expect("instance_pre is intialized before the first call");
            let engine = caller.engine().clone();

            std::thread::Builder::new()
                .name(format!("entry{cpu}"))
                .spawn(move || {
                    let mut store = Store::new(&engine, data);

                    instance_pre
                        .instantiate(&mut store)
                        .unwrap()
                        .get_typed_func::<(u32, u32), ()>(&mut store, "secondary")
                        .expect("the function exists")
                        .call(&mut store, (cpu, idle))
                        .unwrap();
                })?;

            Ok(())
        },
    )?;

    Ok(())
}

fn create_devicetree(cmdline: &str, memory_pages: u32) -> anyhow::Result<Vec<u8>> {
    let mut fdt = FdtWriter::new()?;
    let mut rng_seed = [0u64; 8];
    rand::thread_rng().fill(&mut rng_seed);

    let root = fdt.begin_node("root")?;

    let chosen = fdt.begin_node("chosen")?;
    fdt.property_array_u64("rng-seed", &rng_seed)?;
    fdt.property_string("bootargs", cmdline)?;
    fdt.end_node(chosen)?;

    let aliases = fdt.begin_node("aliases")?;
    fdt.end_node(aliases)?;

    let memory = fdt.begin_node("memory")?;
    fdt.property_string("device_type", "memory")?;
    fdt.property_array_u32("reg", &[0, memory_pages * PAGE_SIZE])?;
    fdt.end_node(memory)?;

    fdt.end_node(root)?;

    Ok(fdt.finish()?)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let engine = Engine::new(
        Config::new()
            .debug_info(args.debug)
            .native_unwind_info(args.debug)
            .wasm_backtrace_details(match args.debug {
                true => WasmBacktraceDetails::Enable,
                false => WasmBacktraceDetails::Disable,
            }),
    )?;

    let memory = SharedMemory::new(&engine, MemoryType::shared(args.memory, args.memory))?;
    debug_assert_eq!(memory.data_size(), (args.memory * PAGE_SIZE) as usize);

    let module = Module::from_file(&engine, &args.module)?;

    let mut store = Store::new(
        &engine,
        State {
            memory: memory.clone(),
            irq: 0,
            devicetree: create_devicetree(&args.cmdline, args.memory)?,
            time_origin: Instant::now(),
            instance_pre: None,
        },
    );

    let mut linker = Linker::new(&engine);
    add_imports(&mut linker, args.debug)?;
    linker.define(&store, "env", "memory", memory)?;

    let instance_pre = linker.instantiate_pre(&module)?;
    store.data_mut().instance_pre = Some(instance_pre.clone());

    instance_pre
        .instantiate(&mut store)?
        .get_typed_func::<(), ()>(&mut store, "boot")
        .expect("the function exists")
        .call(&mut store, ())?;

    Ok(())
}
