# Fixes for building ggml's runtime-dispatched CPU variants (GGML_BACKEND_DL +
# GGML_CPU_ALL_VARIANTS) with clang-cl from cargo-xwin:
# - ggml's MSVC branch omits -mavxvnni for the alderlake variant; MSVC proper
#   needs no target flag for intrinsics, clang-cl does.
# - the xwin toolchain leaves CMAKE_<LANG>_STANDARD_LIBRARIES empty, so the
#   variant DLLs miss advapi32, which ggml-cpu uses to read the CPU name.
include_guard(GLOBAL)
if(NOT WIN32)
    return()
endif()
function(ghostcaret_fix_ggml_cpu_variants)
    get_property(targets DIRECTORY ${CMAKE_SOURCE_DIR}/ggml/src PROPERTY BUILDSYSTEM_TARGETS)
    foreach(t IN LISTS targets)
        if(t MATCHES "^ggml-cpu")
            target_link_libraries(${t} PRIVATE advapi32)
        endif()
    endforeach()
    if(TARGET ggml-cpu-alderlake)
        target_compile_options(ggml-cpu-alderlake PRIVATE -mavxvnni)
    endif()
endfunction()
cmake_language(DEFER DIRECTORY ${CMAKE_SOURCE_DIR} CALL ghostcaret_fix_ggml_cpu_variants)
