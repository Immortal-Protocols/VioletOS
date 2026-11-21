// Read the VioletOS EP0 telemetry snapshot on a directly connected handset.
#include <libusb.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
static uint32_t word(const unsigned char *p) {
  return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 |
         (uint32_t)p[3] << 24;
}
int main(void) {
  libusb_context *ctx = NULL;
  int rc = libusb_init(&ctx);
  if (rc < 0) {
    fprintf(stderr, "%s\n", libusb_error_name(rc));
    return 1;
  }
  libusb_device **list = NULL;
  ssize_t n = libusb_get_device_list(ctx, &list);
  int found = 0;
  for (ssize_t i = 0; i < n; i++) {
    struct libusb_device_descriptor d;
    if (libusb_get_device_descriptor(list[i], &d) || d.idVendor != 0x1209 ||
        d.idProduct != 1)
      continue;
    libusb_device_handle *h;
    rc = libusb_open(list[i], &h);
    if (rc < 0) {
      fprintf(stderr, "%s\n", libusb_error_name(rc));
      continue;
    }
    unsigned char name[128], b[32];
    if (libusb_get_string_descriptor_ascii(h, d.iProduct, name, sizeof name) >
            0 &&
        !strcmp((char *)name, "VioletOS Diagnostics")) {
      rc = libusb_control_transfer(h, 0xc0, 0x51, 0, 0, b, sizeof b, 1000);
      if (rc == sizeof b && !memcmp(b, "VDB1", 4)) {
        printf("{\"counter_ms\":%u,\"battery_mv\":%u,\"wifi_ack_primary\":%u,"
               "\"wifi_ack_secondary\":%u,\"firmware_check\":%u,"
               "\"probe_status\":%u,\"probe_ack\":%u",
               word(b + 4), word(b + 8), word(b + 12), word(b + 16),
               word(b + 20), word(b + 24), word(b + 28));
        unsigned char rt[60];
        int count =
            libusb_control_transfer(h, 0xc0, 0x52, 0, 0, rt, sizeof rt, 1000);
        if (count == sizeof rt) {
          printf(",\"generation\":%u,\"focus_id\":%u,\"last_fault_id\":%u,"
                 "\"runtime_ready\":%u,\"disk_ready\":%u,\"startup_status\":\"",
                 word(rt), word(rt + 4), word(rt + 8), word(rt + 12),
                 word(rt + 16));
          for (unsigned j = 20; j < sizeof rt && rt[j]; j++) {
            unsigned char c = rt[j];
            if (c == '"' || c == '\\')
              putchar('\\');
            if (c >= 32 && c < 127)
              putchar(c);
          }
          putchar('"');
        }
        unsigned char history[8];
        int history_len = libusb_control_transfer(h, 0xc0, 0x56, 0, 0, history, sizeof history, 1000);
        if (history_len == sizeof history && !memcmp(history, "VBH1", 4))
          printf(",\"previous_boot_stage\":%u", word(history + 4));
        unsigned char radio[52];
        count = libusb_control_transfer(h, 0xc0, 0x53, 0, 0, radio,
                                        sizeof radio, 1000);
        if ((count == 20 && !memcmp(radio, "VRD1", 4)) ||
            (count == sizeof radio && !memcmp(radio, "VRD2", 4))) {
          printf(",\"firmware_bytes_staged\":%u,\"chip_id\":%u,\"clock_"
                 "status\":%u,\"rail_ack\":%u",
                 word(radio + 4), word(radio + 8), word(radio + 12),
                 word(radio + 16));
          if (count == sizeof radio)
            printf(",\"rom_mailbox\":%u,\"memory_policy\":%u,\"scan_stage\":%u,"
                   "\"scan_result\":%u,\"beacons_or_responses\":%u,\"channels_"
                   "completed\":%u,\"rf_profile\":%u,\"radio_cleanup\":%u",
                   word(radio + 20), word(radio + 24), word(radio + 28),
                   word(radio + 32), word(radio + 36), word(radio + 40),
                   word(radio + 44), word(radio + 48));
        }
        if (count == sizeof radio && !memcmp(radio, "VRD2", 4)) {
          unsigned char hw[64];
          int got =
              libusb_control_transfer(h, 0xc0, 0x55, 0, 0, hw, sizeof hw, 1000);
          const char *fields[] = {"initial_domain_control",
                                  "initial_spm_config",
                                  "initial_power_ack_a",
                                  "initial_power_ack_b",
                                  "initial_bus_protection",
                                  "connectivity_hw_version",
                                  "connectivity_fw_version",
                                  "wmt_command",
                                  "wmt_reply_header",
                                  "wmt_reply_status",
                                  "dma_tx",
                                  "dma_control",
                                  "dma_device_index",
                                  "wlan_event",
                                  "wlan_result"};
          if (got == sizeof hw && !memcmp(hw, "VRG1", 4))
            for (unsigned j = 0; j < 15; j++)
              printf(",\"%s\":%u", fields[j], word(hw + 4 + 4 * j));
        }
        if (count == sizeof radio && !memcmp(radio, "VRD2", 4) &&
            word(radio + 32) == 43) {
          unsigned char name[37];
          int got = libusb_control_transfer(h, 0xc0, 0x54, 0, 0, name,
                                            sizeof name, 1000);
          if (got == sizeof name && !memcmp(name, "VSN1", 4) && name[4] <= 32) {
            printf(",\"first_ssid\":\"");
            for (unsigned j = 5; j < 5u + name[4]; j++) {
              unsigned c = name[j];
              if (c == '"' || c == 92)
                putchar(92);
              putchar(c >= 32 && c < 127 ? (int)c : '?');
            }
            putchar('"');
          }
        }
        if (count == sizeof radio) {
          unsigned char mpu[40];
          int n = libusb_control_transfer(h, 0xc0, 0x57, 0, 0, mpu, sizeof mpu, 1000);
          if ((n == sizeof mpu || n == 36) && !memcmp(mpu, "MPU1", 4)) {
            if (n == sizeof mpu)
              printf(",\"wifi_memory_checks\":%u", word(mpu + 36));
            for (unsigned i=0; i<2; ++i) {
              const unsigned char *r = mpu + 4 + i*16;
              printf(",\"mpu_region_%u\":{\"start_page\":%u,\"end_page\":%u,"
                     "\"permissions_low\":%u,\"permissions_high\":%u}",
                     i ? 29 : 25, word(r), word(r+4), word(r+8), word(r+12));
            }
          }
        }
        puts("}");
        found++;
      } else {
        fprintf(stderr, "Invalid USB status reply (%d)\n", rc);
      }
    }
    libusb_close(h);
  }
  if (!found)
    fprintf(stderr, "No VioletOS diagnostic device responded.\n");
  if (list)
    libusb_free_device_list(list, 1);
  libusb_exit(ctx);
  return found ? 0 : 1;
}
