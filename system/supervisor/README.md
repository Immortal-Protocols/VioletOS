# Process supervisor

The proposed supervisor runs independently of Runtime and monitors the launcher, Runtime and services. On a managed process fault, it would restore the group and its connections in dependency order. Test recovery by stopping a service in QEMU and checking that Home returns and the launcher can open an app.
