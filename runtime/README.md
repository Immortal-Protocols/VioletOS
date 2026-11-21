# Runtime

The proposed Runtime process resolves app identities, starts or resumes processes, and exposes the stable interface used by apps. Initially it forwards authorized Subscribe, Get and Execute requests to the underlying service and preserves the caller’s identity. A compatibility test should replace a service and run the same app binary against it.
