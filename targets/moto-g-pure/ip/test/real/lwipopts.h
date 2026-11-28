// lwIP's options for the host test of sockets.c against lwIP itself
// (test_lwip.c): Radio's own (../../lwipopts.h), with only what a 64-bit host
// and hand-made packets need changed.
#pragma once
#include "../../lwipopts.h"
#undef MEM_ALIGNMENT
#define MEM_ALIGNMENT 8
// The test's peer writes no checksums.
#define CHECKSUM_CHECK_IP 0
#define CHECKSUM_CHECK_TCP 0
#define CHECKSUM_CHECK_UDP 0
