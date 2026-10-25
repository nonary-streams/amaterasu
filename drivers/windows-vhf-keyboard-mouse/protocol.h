// Copyright (c) 2026 Chase Payne
// SPDX-License-Identifier: MIT
// Fixed-size private control ABI. This interface is available only to the
// separately installed companion; it never accepts arbitrary HID descriptors.
#pragma once

#include <cstdint>
#include <windows.h>
#include <winioctl.h>

namespace amaterasu::vhf {

inline constexpr std::uint16_t protocol_version = 1;
inline constexpr GUID interface_guid {
  0xb539be52, 0xb2c5, 0x462a, {0x98, 0xc2, 0xc4, 0xa9, 0xc8, 0x49, 0xcb, 0x56}
};
inline constexpr wchar_t hardware_id[] = L"ROOT\\AMATERASUVHFINPUT";

struct request_header {
  std::uint16_t size;
  std::uint16_t version;
  std::uint32_t reserved;
};

struct basic_request {
  request_header header;
};

struct query_response {
  request_header header;
  std::uint32_t capabilities;
  std::uint32_t maximum_pending_reports;
};

struct keyboard_request {
  request_header header;
  std::uint8_t modifiers;
  std::uint8_t keys[6];
  std::uint8_t reserved;
};

struct mouse_request {
  request_header header;
  std::uint8_t buttons;
  std::uint8_t reserved0;
  std::int16_t delta_x;
  std::int16_t delta_y;
  std::int16_t wheel;
  std::int16_t pan;
  std::uint16_t reserved1;
};

inline constexpr ULONG query = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS);
inline constexpr ULONG create = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_WRITE_ACCESS);
inline constexpr ULONG keyboard = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_WRITE_ACCESS);
inline constexpr ULONG mouse = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_WRITE_ACCESS);
inline constexpr ULONG destroy = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, FILE_WRITE_ACCESS);
inline constexpr ULONG heartbeat = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x806, METHOD_BUFFERED, FILE_WRITE_ACCESS);

static_assert(sizeof(request_header) == 8);
static_assert(sizeof(basic_request) == 8);
static_assert(sizeof(query_response) == 16);
static_assert(sizeof(keyboard_request) == 16);
static_assert(sizeof(mouse_request) == 20);

}  // namespace amaterasu::vhf
