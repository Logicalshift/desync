# Desync

This library provides an alternative synchronisation mechanism to the usual threads/mutexes
system. Instead of managing the threads, this focuses on managing the data, which largely
does away with the need for many synchronisation primitives. Support for futures is provided
to help interoperate with other Rust libraries.

The main difference in approach is that while a 'traditional' mutual-exclusion based
model focuses on managing access to data, this instead focuses on ordering access. Only
one job can access data at a time because the jobs are always done in order. Defining
the order of operations ahead of time makes it easier to reason about the runtime behaviour
of a particular data structure.

There is a single new synchronisation object: `Desync`. You create one like this:

```Rust
use desync::Desync;
let number = Desync::new(0);
```

It supports two main operations. `async` will schedule a new job for the object that will run
in a background thread. It's useful for deferring long-running operations and moving updates
so they can run in parallel.

```Rust
let number = Desync::new(0);
number.async(|val| {
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

There's one final operation to be aware of and that's `future`. This returns a boxed Future that
can be used with other libraries that use them. It's conceptually the same as `sync`, except that
it doesn't wait for the operation to complete:

```Rust
let future_number = number.future(|val| *val);
assert!(executor::spawn(future_number).wait_future().unwrap() == 42);
```
