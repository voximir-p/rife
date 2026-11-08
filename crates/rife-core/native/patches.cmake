# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only

# Every source overlay must match independently. A different successful patch
# must never hide a missing safety/cancellation guard in an unsupported source.
function(rife_replace_checked variable description anchor replacement)
    set(expected_count 1)
    if(ARGC GREATER 4)
        set(expected_count "${ARGV4}")
    endif()
    string(REPLACE "${anchor}" "" without_anchors "${${variable}}")
    string(LENGTH "${${variable}}" source_length)
    string(LENGTH "${without_anchors}" remaining_length)
    string(LENGTH "${anchor}" anchor_length)
    math(EXPR count "(${source_length} - ${remaining_length}) / ${anchor_length}")
    if(NOT count EQUAL expected_count)
        message(FATAL_ERROR "Unsupported NCNN source: expected ${expected_count} ${description} anchors, found ${count}")
    endif()
    string(REPLACE "${anchor}" "${replacement}" patched "${${variable}}")
    set(${variable} "${patched}" PARENT_SCOPE)
endfunction()

function(rife_regex_replace_checked variable description pattern replacement expected_count)
    string(REGEX MATCHALL "${pattern}" matches "${${variable}}")
    list(LENGTH matches count)
    if(NOT count EQUAL expected_count)
        message(FATAL_ERROR "Unsupported NCNN source: expected ${expected_count} ${description} anchors, found ${count}")
    endif()
    string(REGEX REPLACE "${pattern}" "${replacement}" patched "${${variable}}")
    set(${variable} "${patched}" PARENT_SCOPE)
endfunction()
