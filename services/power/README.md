# Power service

The proposed power process reports battery state and handles authorized requests to change device power state. Runtime should distinguish permission to read the battery level from permission to request sleep or shutdown. Test those permissions with a fake power controller before exercising the handset.
