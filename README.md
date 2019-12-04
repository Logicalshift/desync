# Desync

```toml
[dependencies]
desync = "0.5"
```

Desync provides a single type, `Desync<T>` that can be used to replace both threads and mutexes.
This type schedules operations for a contained data structure so that they are always performed
in order and optionally in the background.

Such a `Desync` object can be created like so:

```Rust
use desync::Desync;
let number = Desync::new(0);
```

It supports two main operations. `async` will schedule a new job for the object that will run
in a background thread. It's useful for deferring long-running operations and moving updates
so they can run in parallel.

```Rust
let number = Desync::new(0);
number.desync(|val| {
    // Long update here
    thread::sleep(Duration::from_millis(100));
    *val = 42;
});

// We can carry on what we're doing with the update now running in the background
```

The other operation is `sync`, which schedules a job to run synchronously on the data structure.
This is useful for retrieving values from a `Desync`.

```Rust
let new_number = number.sync(|val| *val);           // = 42
```

`Desync` objects always run operations in the order that is provided, so all operations are
serialized from the point of view of the data that they contain. When combined with the ability
to perform operations asynchronously, this provides a useful way to immediately parallelize
long-running operations.

# Working with futures

Desync has support for the `futures` library. The simplest operation is `future()`, which creates
a future that runs asynchronously on a `Desync` object but - unlike `desync()` can return a result.
It works like this:

```Rust
let future_number = number.future(|val| *val);
assert!(executor::block_on(async { future_number.await.unwrap() == 42 }))
```

There is also support for streams, via the `pipe_in()` and `pipe()` functions. These work on
`Arc<Desync<T>>` references and provide a way to process a stream asynchronously. These two
functions provide a powerful way to process input and also to connect `Desync` objects together
using message-passing for communication.

```Rust
let some_object = Arc::new(Desync::new(some_object));

pipe_in(Arc::clone(&number), some_stream, 
    |some_object, input| some_object.process(input));

let output_stream = pipe(Arc::clone(&number), some_stream, 
    |some_object, input| some_object.process_with_output(input));
```
