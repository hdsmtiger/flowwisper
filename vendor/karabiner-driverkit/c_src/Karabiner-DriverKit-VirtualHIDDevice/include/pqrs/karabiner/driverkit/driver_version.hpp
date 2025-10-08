#pragma once

#include <compare>
#include <iostream>
#include <type_safe/strong_typedef.hpp>

namespace pqrs {
namespace karabiner {
namespace driverkit {
namespace driver_version {
struct value_t : type_safe::strong_typedef<value_t, uint64_t>,
                 type_safe::strong_typedef_op::equality_comparison<value_t>,
                 type_safe::strong_typedef_op::relational_comparison<value_t> {
  using strong_typedef::strong_typedef;

  constexpr auto operator<=>(const value_t& other) const {
    return type_safe::get(*this) <=> type_safe::get(other);
  }
};

inline std::ostream& operator<<(std::ostream& stream, const value_t& value) {
  return stream << type_safe::get(value);
}

// clang-format off
constexpr value_t embedded_driver_version(10800);
// clang-format on
} // namespace driver_version
} // namespace driverkit
} // namespace karabiner
} // namespace pqrs
