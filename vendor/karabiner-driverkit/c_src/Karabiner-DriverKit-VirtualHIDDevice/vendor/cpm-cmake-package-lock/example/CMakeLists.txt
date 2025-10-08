cmake_minimum_required(VERSION 3.24 FATAL_ERROR)
project(vendor NONE)

include("${CMAKE_CURRENT_LIST_DIR}/../vendor/CPM.cmake")

CPMUsePackageLock("${CMAKE_CURRENT_LIST_DIR}/../cmake/package-lock.cmake")

set(VENDOR_INCLUDE_DIR "${CMAKE_CURRENT_LIST_DIR}/vendor")
file(MAKE_DIRECTORY "${VENDOR_INCLUDE_DIR}")

include("${CMAKE_CURRENT_LIST_DIR}/../cmake/common.cmake")
include("${CMAKE_CURRENT_LIST_DIR}/../cmake/dependency.cmake")

copy_vendor_package(pqrs_cf_json)

# copy_vendor_package(asio)
# copy_vendor_package(cxxopts)
# copy_vendor_package(debug_assert)
# copy_vendor_package(eternal)
# copy_vendor_package(exprtk)
# copy_vendor_package(glob)
# copy_vendor_package(gsl)
# copy_vendor_package(natural_sort)
# copy_vendor_package(pqrs_osx_frontmost_application_monitor)
# copy_vendor_package(pqrs_osx_process_info)
# copy_vendor_package(pqrs_osx_workspace)
# copy_vendor_package(utfcpp)
