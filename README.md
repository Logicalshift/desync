# Desync

```toml
[dependencies]
desync = "0.9"
```

Desync provides a new synchronisation type, `Desync<T>`, which works by ordering operations on
its enclosed data type instead of the traditional method of using mutexes to protect critical
sections. This allows concurrency to be built around two basic operations:

 * `desync_thing.sync(|thing| /* ... */)` for synchronous access to the data
 * `desync_thing.desync(|thing| /* ... */)` for asynchronous access to the data - running the supplied task in the background.

If only the `sync()` operation is used, this is roughly equivalent to a standard `Mutex`, except
with much stronger guarantees about which thread gets the data first. The other operation,
`desync()` effectively replaces the need to spawn threads and move data around in order to 
add concurrency to a program.

Desync also provides equivalent methods for async code: `future_sync()` will perform an operation
in the current async context and `future_desync()` will schedule an operation in the background.
These can be freely mixed with the `sync()` and `desync()` operations so it becomes fairly easy to
mix code using traditional threading and code using async futures. As Desync uses order-of-operations
to guarantee exclusive access to the data, these operations can borrow the contained data across any
`await`s that might be needed, unlike locks created using the `Mutex` type, which can't be sent
between threads.

Desync provides fairly strong ordering guarantees: in particular, when any of the methods return,
the ordering of the operation is guaranteed relative to any following operation. This property makes
desync code quite easy to follow and less prone to race conditions than traditional threading. The
ability to easily schedule updates asynchronously provides a way around common scenarios where the
need to lock multiple mutexes can create deadlocks.

# Quick start

Desync provides a single type, `Desync<T>` that can be used to replace both threads and mutexes.
This type schedules operations for a contained data structure so that they are always performed
in order and optionally in the background.

Such a `Desync` object can be created like so:

```Rust
use desync::Desync;
let number = Desync::new(0);
```

It supports two main operations. `desync` will schedule a new job for the object that will run
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

Desync has support for the `futures` library. The simplest operation is `future_sync()`, which 
creates a future that runs asynchronously on a `Desync` object but - unlike `desync()` can 
return a result. It works like this:

```Rust
let future_number = number.future_sync(|val| future::ready(*val).boxed());
assert!(executor::block_on(async { future_number.await.unwrap() }) == 42 )
```

There is also a `future_desync()` operation, which can be used in cases where the thread is
expected to block. It can be used in the same situations as `future_sync()` but has a `detach()`
method to leave the task running in the background, or a `sync()` method to wait for the result
to be computed.

Desync can run streams in the background too, via the `pipe_in()` and `pipe()` functions. These 
work  on `Arc<Desync<T>>` references and provide a way to process a stream asynchronously. These 
two functions provide a powerful way to process input and also to connect `Desync` objects 
together using message-passing for communication.

```Rust
let some_object = Arc::new(Desync::new(some_object));

pipe_in(Arc::clone(&number), some_stream, 
    |some_object, input| some_object.process(input));

let output_stream = pipe(Arc::clone(&number), some_stream, 
    |some_object, input| some_object.process_with_output(input));
```
