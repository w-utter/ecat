ethercat maindevice completely driven using io_uring.
- single threaded userspace application
- multishot recv using raw linux sockets

initally based off of the io_uring driver in ethercrab

the spin example will drive a motor in CSV according to ds204
