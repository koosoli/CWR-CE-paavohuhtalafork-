# CopyIfExists.cmake — copy SRC into directory DST only when SRC exists.
#
# Used for artifacts that are only produced in some build profiles (e.g. the Rust
# cdylib's PDB exists in dbg/rwdi but not rel), so an unconditional copy_if_different
# would fail the build when the source is absent. Invoke with -P:
#   ${CMAKE_COMMAND} -DSRC=<file> -DDST=<dir> -P cmake/CopyIfExists.cmake
if(EXISTS "${SRC}")
    execute_process(COMMAND ${CMAKE_COMMAND} -E make_directory "${DST}")
    execute_process(COMMAND ${CMAKE_COMMAND} -E copy_if_different "${SRC}" "${DST}")
endif()
