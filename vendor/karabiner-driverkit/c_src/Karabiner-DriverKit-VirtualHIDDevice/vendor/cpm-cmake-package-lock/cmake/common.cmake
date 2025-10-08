get_filename_component(_HOME_DIR "~" REALPATH)
set(CPM_SOURCE_CACHE "${_HOME_DIR}/.local/cpm-cmake/source_cache")

add_custom_target(vendor_files ALL)

function(copy_vendor_package PKG)
  #
  # Check if it has already been copied
  #

  get_property(done GLOBAL PROPERTY _COPIED_PACKAGES)
  list(FIND done "${PKG}" _idx)
  if(_idx GREATER -1)
    return()
  endif()
  list(APPEND done "${PKG}")
  set_property(GLOBAL PROPERTY _COPIED_PACKAGES "${done}")

  #
  # git clone
  #

  CPMAddPackage(NAME ${PKG})

  #
  # Copy files
  #

  if(DEFINED _FILES_${PKG})
    set(_files ${_FILES_${PKG}})
  else()
    set(_files "include -> /")
  endif()

  foreach(_file IN LISTS _files)
    string(REPLACE "->" ";" _pair ${_file})
    list(TRANSFORM _pair STRIP)
    list(GET _pair 0 _src)
    list(GET _pair 1 _dst)

    set(_src "${${PKG}_SOURCE_DIR}/${_src}")
    set(_dst "${VENDOR_INCLUDE_DIR}/${_dst}")

    cmake_path(GET _dst PARENT_PATH _dst_dir)
    file(MAKE_DIRECTORY ${_dst_dir})

    add_custom_command(
      TARGET vendor_files PRE_BUILD
      COMMENT "Vendoring ${PKG}: \n  src: ${_src}\n  dst: ${_dst}"
      COMMAND cp -r "${_src}" "${_dst}"
    )
  endforeach()

  #
  # Handle dependencies
  #

  if(DEFINED _DEPS_${PKG})
    foreach(dep IN LISTS _DEPS_${PKG})
      copy_vendor_package("${dep}")
    endforeach()
  endif()
endfunction()
