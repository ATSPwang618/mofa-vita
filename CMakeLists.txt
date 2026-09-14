cmake_minimum_required(VERSION 3.20)
project(mofa-vita-krkr VERSION 1.0.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

option(MOFA_BUILD_TESTS "Build host-side tests" ON)
option(MOFA_ENABLE_OPENGL_COMPOSITOR
    "Use Yuri's experimental OpenGL compositor instead of software rendering"
    OFF)
set(MOFA_MS_GOTHIC_TTC "" CACHE FILEPATH
    "Optional local msgothic.ttc used only by the host golden test")

set(BUILD_SHARED_LIBS OFF CACHE BOOL "Build shared libraries" FORCE)
set(BUILD_TEST OFF CACHE BOOL "Build dependency tests" FORCE)
set(INSTALL_DOCUMENTATION OFF CACHE BOOL "Install dependency documentation" FORCE)
set(INSTALL_EXAMPLES OFF CACHE BOOL "Install dependency examples" FORCE)
include(cmake/Dependencies.cmake)

if(VITA AND NOT CMAKE_BUILD_TYPE STREQUAL "Release")
    message(FATAL_ERROR
        "mofa-vita-krkr packages must use -DCMAKE_BUILD_TYPE=Release")
endif()

set(MOFA_YURI_TJS_SOURCE_DIR
    "${MOFA_YURI_SOURCE_DIR}/src/core/tjs2")
set(MOFA_YURI_TJS_GENERATED_DIR
    "${CMAKE_CURRENT_BINARY_DIR}/generated/yuri/tjs2")
file(MAKE_DIRECTORY "${MOFA_YURI_TJS_GENERATED_DIR}")

# Yuri's layer-tree traversal takes a safe lock for every child list during
# completion.  Its Compact() implementation used to replace the backing array
# even when the list contained no null entries, turning every read-only walk
# into a new[]/delete[] pair.  Generate a Vita overlay that preserves the
# existing compaction path after a removal while retaining stable storage for
# an unchanged list.  The host-only regression test below explicitly includes
# this overlay; ordinary host engine targets continue to use upstream Yuri.
set(MOFA_YURI_OBJECT_LIST_SOURCE
    "${MOFA_YURI_SOURCE_DIR}/src/core/utils/ObjectList.h")
file(READ "${MOFA_YURI_OBJECT_LIST_SOURCE}"
    MOFA_YURI_OBJECT_LIST_TEXT)
set(MOFA_YURI_OBJECT_LIST_COMPACT_OLD [=[
		Count = (tjs_int)(d - Objects);
		Capacity = Count;
]=])
set(MOFA_YURI_OBJECT_LIST_COMPACT_STABLE [=[
		const tjs_int compacted_count = (tjs_int)(d - Objects);
		if(compacted_count == Count) return;
		Count = compacted_count;
		Capacity = Count;
]=])
string(REPLACE
    "${MOFA_YURI_OBJECT_LIST_COMPACT_OLD}"
    "${MOFA_YURI_OBJECT_LIST_COMPACT_STABLE}"
    MOFA_YURI_OBJECT_LIST_PATCHED
    "${MOFA_YURI_OBJECT_LIST_TEXT}")
if(MOFA_YURI_OBJECT_LIST_PATCHED STREQUAL
   MOFA_YURI_OBJECT_LIST_TEXT)
    message(FATAL_ERROR
        "Yuri ObjectList stable Compact patch no longer applies")
endif()
string(FIND "${MOFA_YURI_OBJECT_LIST_PATCHED}"
    "${MOFA_YURI_OBJECT_LIST_COMPACT_OLD}"
    MOFA_YURI_OBJECT_LIST_OLD_OFFSET)
if(NOT MOFA_YURI_OBJECT_LIST_OLD_OFFSET LESS 0)
    message(FATAL_ERROR
        "Yuri ObjectList generated header retains the allocating Compact path")
endif()
foreach(MOFA_YURI_OBJECT_LIST_CONTRACT
        "const tjs_int compacted_count = (tjs_int)(d - Objects);"
        "if(compacted_count == Count) return;"
        "Count = compacted_count;")
    string(FIND "${MOFA_YURI_OBJECT_LIST_PATCHED}"
        "${MOFA_YURI_OBJECT_LIST_CONTRACT}"
        MOFA_YURI_OBJECT_LIST_CONTRACT_OFFSET)
    if(MOFA_YURI_OBJECT_LIST_CONTRACT_OFFSET LESS 0)
        message(FATAL_ERROR
            "Yuri ObjectList generated-header contract is incomplete: ${MOFA_YURI_OBJECT_LIST_CONTRACT}")
    endif()
endforeach()
set(MOFA_YURI_OBJECT_LIST_GENERATED
    "${CMAKE_CURRENT_BINARY_DIR}/generated/yuri/ObjectList.h")
file(CONFIGURE
    OUTPUT "${MOFA_YURI_OBJECT_LIST_GENERATED}"
    CONTENT "${MOFA_YURI_OBJECT_LIST_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)
# Compile a generated copy of the pinned TJS unit. Several Yuri fixes affect
# inline headers, so generating only patched .cpp files would leave different
# semantics in callers from the base, visual, sound and plugin archives.
file(COPY "${MOFA_YURI_TJS_SOURCE_DIR}/"
    DESTINATION "${MOFA_YURI_TJS_GENERATED_DIR}"
    PATTERN "tjsConfig.cpp" EXCLUDE
    PATTERN "tjsString.h" EXCLUDE
    PATTERN "tjsVariant.h" EXCLUDE
    PATTERN "tjsVariant.cpp" EXCLUDE)

# Bytecode constants may contain ordinary Dictionary/Array dispatch objects as
# well as tTJSInterCodeContext objects.  Yuri's exporter used a C-style
# downcast before merely comparing pointer identity, which is undefined for an
# ordinary dispatch object and is diagnosed by UBSan on real retail scripts.
# Classify through the polymorphic hierarchy first; only an actual inter-code
# context can be looked up in the script block's context table.
set(MOFA_TJS_CONST_ARRAY_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsConstArrayData.cpp")
file(READ "${MOFA_TJS_CONST_ARRAY_SOURCE}"
    MOFA_TJS_CONST_ARRAY_TEXT)
set(MOFA_TJS_CONST_ARRAY_BAD_LOOKUP [=[
		iTJSDispatch2* obj = v.AsObjectNoAddRef();
		int index = block->GetCodeIndex( (const tTJSInterCodeContext*)obj );
]=])
set(MOFA_TJS_CONST_ARRAY_SAFE_LOOKUP [=[
		iTJSDispatch2* obj = v.AsObjectNoAddRef();
		const tTJSInterCodeContext* context =
			dynamic_cast<const tTJSInterCodeContext*>(obj);
		int index = context ? block->GetCodeIndex(context) : -1;
]=])
string(REPLACE
    "${MOFA_TJS_CONST_ARRAY_BAD_LOOKUP}"
    "${MOFA_TJS_CONST_ARRAY_SAFE_LOOKUP}"
    MOFA_TJS_CONST_ARRAY_PATCHED
    "${MOFA_TJS_CONST_ARRAY_TEXT}")
if(MOFA_TJS_CONST_ARRAY_PATCHED STREQUAL
   MOFA_TJS_CONST_ARRAY_TEXT)
    message(FATAL_ERROR
        "Yuri bytecode object-constant classification patch no longer applies")
endif()
set(MOFA_TJS_CONST_ARRAY_BAD_PUT [=[
	case TYPE_INTER_OBJECT: {
		iTJSDispatch2* obj = v.AsObjectNoAddRef();
		return block->GetCodeIndex( (const tTJSInterCodeContext*)obj );
	}
]=])
set(MOFA_TJS_CONST_ARRAY_SAFE_PUT [=[
	case TYPE_INTER_OBJECT: {
		iTJSDispatch2* obj = v.AsObjectNoAddRef();
		const tTJSInterCodeContext* context =
			dynamic_cast<const tTJSInterCodeContext*>(obj);
		return context ? block->GetCodeIndex(context) : -1;
	}
]=])
set(MOFA_TJS_CONST_ARRAY_BEFORE_PUT
    "${MOFA_TJS_CONST_ARRAY_PATCHED}")
string(REPLACE
    "${MOFA_TJS_CONST_ARRAY_BAD_PUT}"
    "${MOFA_TJS_CONST_ARRAY_SAFE_PUT}"
    MOFA_TJS_CONST_ARRAY_PATCHED
    "${MOFA_TJS_CONST_ARRAY_PATCHED}")
if(MOFA_TJS_CONST_ARRAY_PATCHED STREQUAL
   MOFA_TJS_CONST_ARRAY_BEFORE_PUT)
    message(FATAL_ERROR
        "Yuri bytecode inter-code constant insertion patch no longer applies")
endif()
string(FIND "${MOFA_TJS_CONST_ARRAY_PATCHED}"
    "(const tTJSInterCodeContext*)obj"
    MOFA_TJS_CONST_ARRAY_UNSAFE_CAST_OFFSET)
if(NOT MOFA_TJS_CONST_ARRAY_UNSAFE_CAST_OFFSET LESS 0)
    message(FATAL_ERROR
        "Yuri bytecode object-constant exporter retains an unsafe downcast")
endif()
file(CONFIGURE
    OUTPUT "${MOFA_YURI_TJS_GENERATED_DIR}/tjsConstArrayData.cpp"
    CONTENT "${MOFA_TJS_CONST_ARRAY_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

# Yuri's hash table uses placement new into raw byte arrays, but those arrays
# do not carry the alignment of KeyT/ValueT. This is undefined behavior for
# cached script objects on 64-bit hosts and for any over-aligned value on the
# target. Preserve the table layout and lifetime scheme while giving both
# placement-new destinations their required C++ alignment.
set(MOFA_TJS_HASH_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsHashSearch.h")
file(READ "${MOFA_TJS_HASH_SOURCE}" MOFA_TJS_HASH_TEXT)
set(MOFA_TJS_HASH_BAD_STORAGE
    "\t\tchar Key[sizeof(KeyT)];\n\t\tchar Value[sizeof(ValueT)];")
set(MOFA_TJS_HASH_FIXED_STORAGE
    "\t\talignas(KeyT) char Key[sizeof(KeyT)];\n\t\talignas(ValueT) char Value[sizeof(ValueT)];")
string(REPLACE "${MOFA_TJS_HASH_BAD_STORAGE}"
    "${MOFA_TJS_HASH_FIXED_STORAGE}"
    MOFA_TJS_HASH_PATCHED "${MOFA_TJS_HASH_TEXT}")
if(MOFA_TJS_HASH_PATCHED STREQUAL MOFA_TJS_HASH_TEXT)
    message(FATAL_ERROR "Yuri hash-table placement alignment patch no longer applies")
endif()
file(CONFIGURE
    OUTPUT "${MOFA_YURI_TJS_GENERATED_DIR}/tjsHashSearch.h"
    CONTENT "${MOFA_TJS_HASH_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

# Yuri's bundled UTF-16 formatter contains two target-ABI assumptions. Generate
# a corrected translation unit while leaving the pinned upstream source intact.
set(MOFA_TJS_CONFIG_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsConfig.cpp")
file(READ "${MOFA_TJS_CONFIG_SOURCE}" MOFA_TJS_CONFIG_TEXT)

# LDBL_MANT_DIG is 53 on Vita, so shifting an int by that amount is undefined.
# ldexpl expresses the intended 2^mantissa value without an integer-width
# dependency.
string(REPLACE
    "long double round = 0x1<<LDBL_MANT_DIG;"
    "long double round = ldexpl(1.0L, LDBL_MANT_DIG);"
    MOFA_TJS_CONFIG_PATCHED "${MOFA_TJS_CONFIG_TEXT}")
if(MOFA_TJS_CONFIG_PATCHED STREQUAL MOFA_TJS_CONFIG_TEXT)
    message(FATAL_ERROR "Yuri tjsConfig long-double patch no longer applies")
endif()

# A va_list function parameter is already adjusted on x86_64 SysV; taking its
# address creates a pointer-to-pointer and crashes while formatting errors.
if(NOT VITA)
    set(MOFA_BAD_VA_LIST
        "va_list *pap = (va_list *)&ap;\n    r = printf_core(&f, fmt, pap, nl_arg, nl_type);")
    set(MOFA_FIXED_VA_LIST
        "va_list copied;\n    va_copy(copied, ap);\n    r = printf_core(&f, fmt, &copied, nl_arg, nl_type);\n    va_end(copied);")
    string(REPLACE "${MOFA_BAD_VA_LIST}" "${MOFA_FIXED_VA_LIST}"
        MOFA_TJS_CONFIG_HOST_PATCHED "${MOFA_TJS_CONFIG_PATCHED}")
    if(MOFA_TJS_CONFIG_HOST_PATCHED STREQUAL MOFA_TJS_CONFIG_PATCHED)
        message(FATAL_ERROR "Yuri tjsConfig va_list patch no longer applies")
    endif()
    set(MOFA_TJS_CONFIG_PATCHED "${MOFA_TJS_CONFIG_HOST_PATCHED}")
endif()

# TJS_malloc stores an allocation size immediately before the returned
# pointer. TJS_free must nevertheless retain the standard free(nullptr)
# contract: the variant-array stack legitimately has no backing allocation
# when an engine is created only for compilation. Subtracting the header size
# from null is undefined and crashed during normal tTJS destruction.
set(MOFA_TJS_CONFIG_BAD_FREE
    "void TJS_free(void *buf)\n{\n\tfree((char*)buf - sizeof(size_t));\n}")
set(MOFA_TJS_CONFIG_FIXED_FREE
    "void TJS_free(void *buf)\n{\n\tif (!buf) return;\n\tfree((char*)buf - sizeof(size_t));\n}")
string(REPLACE "${MOFA_TJS_CONFIG_BAD_FREE}"
    "${MOFA_TJS_CONFIG_FIXED_FREE}"
    MOFA_TJS_CONFIG_ALLOCATOR_PATCHED "${MOFA_TJS_CONFIG_PATCHED}")
if(MOFA_TJS_CONFIG_ALLOCATOR_PATCHED STREQUAL MOFA_TJS_CONFIG_PATCHED)
    message(FATAL_ERROR "Yuri null-free allocator patch no longer applies")
endif()
set(MOFA_TJS_CONFIG_PATCHED "${MOFA_TJS_CONFIG_ALLOCATOR_PATCHED}")
file(CONFIGURE
    OUTPUT "${MOFA_YURI_TJS_GENERATED_DIR}/tjsConfig.cpp"
    CONTENT "${MOFA_TJS_CONFIG_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

# Yuri's empty ttstr representation is a null internal string pointer.  Its
# GetLen() null check was historically restricted to CodeGuard builds, which
# makes ordinary optimized builds dereference null for a perfectly valid empty
# string (for example while formatting an error with an empty media name).
# Generate a corrected copy while keeping the pinned submodule immutable. The
# generated include directory is used by every backend archive, so inline
# string semantics cannot differ across static-library boundaries.
set(MOFA_TJS_STRING_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsString.h")
file(READ "${MOFA_TJS_STRING_SOURCE}" MOFA_TJS_STRING_TEXT)
set(MOFA_TJS_STRING_BAD_GETLEN
    "#ifdef __CODEGUARD__\n\t\tif(!Ptr) return 0; // tTJSVariantString::GetLength can return zero if 'this' is NULL\n#endif\n\t\treturn Ptr->GetLength();")
set(MOFA_TJS_STRING_FIXED_GETLEN
    "if(!Ptr) return 0;\n\t\treturn Ptr->GetLength();")
string(REPLACE "${MOFA_TJS_STRING_BAD_GETLEN}"
    "${MOFA_TJS_STRING_FIXED_GETLEN}"
    MOFA_TJS_STRING_PATCHED "${MOFA_TJS_STRING_TEXT}")
if(MOFA_TJS_STRING_PATCHED STREQUAL MOFA_TJS_STRING_TEXT)
    message(FATAL_ERROR "Yuri empty-string length patch no longer applies")
endif()
set(MOFA_TJS_STRING_COMPAT_HEADER
    "${MOFA_YURI_TJS_GENERATED_DIR}/tjsString.h")
file(CONFIGURE
    OUTPUT "${MOFA_TJS_STRING_COMPAT_HEADER}"
    CONTENT "${MOFA_TJS_STRING_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

# Apply the same empty-string invariant to tTJSVariant. Empty strings are
# represented by tvtString plus a null String pointer; every conversion must
# therefore produce the normal empty value instead of invoking a member on
# null. This is exercised by the real-script bytecode compiler test below.
set(MOFA_TJS_VARIANT_HEADER_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsVariant.h")
file(READ "${MOFA_TJS_VARIANT_HEADER_SOURCE}"
    MOFA_TJS_VARIANT_HEADER_TEXT)
string(REPLACE
    "\t\treturn *String;"
    "\t\treturn String ? String->operator const tjs_char *() : TJSNullStrPtr;"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_TEXT}")
string(REPLACE
    "case tvtString:  return String->ToInteger();"
    "case tvtString:  return String ? String->ToInteger() : 0;"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "case tvtString:  String->ToNumber(targ); return;"
    "case tvtString:  if(String) String->ToNumber(targ); else targ = (tjs_int)0; return;"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "case tvtString:  return String->ToReal();"
    "case tvtString:  return String ? String->ToReal() : 0.0;"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "\t\t\tString->ToNumber(val);"
    "\t\t\tif(String) String->ToNumber(val); else val = (tjs_int)0;"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
set(MOFA_TJS_VARIANT_HEADER_BEFORE_CONCAT
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "\t\t\tval.String = TJSAllocVariantString(*s1, *s2);"
    "\t\t\tval.String = TJSAllocVariantString(\n\t\t\t\ts1 ? s1->operator const tjs_char *() : TJSNullStrPtr,\n\t\t\t\ts2 ? s2->operator const tjs_char *() : TJSNullStrPtr);"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
if(MOFA_TJS_VARIANT_HEADER_PATCHED STREQUAL
   MOFA_TJS_VARIANT_HEADER_BEFORE_CONCAT)
    message(FATAL_ERROR "Yuri empty-string binary-concatenation patch no longer applies")
endif()

# TJS exposes signed 64-bit integers, but its historical desktop runtime also
# inherits x86's modulo-64 variable-shift count.  Express that established bit
# behavior without signed-overflow, invalid-count, or signed-right-shift C++
# behavior so the compiler's constant folder and every runtime caller agree on
# x86, ARM and sanitizer builds.
set(MOFA_TJS_VARIANT_SHIFT_DECLARATIONS [=[
TJS_EXP_FUNC_DEF(void, TJSThrowDivideByZero, ());
]=])
set(MOFA_TJS_VARIANT_DEFINED_SHIFTS [=[
TJS_EXP_FUNC_DEF(void, TJSThrowDivideByZero, ());

static inline tjs_uint TJSNormalizeShiftCount(tTVInteger count)
{
	return static_cast<tjs_uint>(static_cast<tjs_uint64>(count) & 63u);
}

static inline tTVInteger TJSIntegerFromBitPattern(tjs_uint64 bits)
{
	const tjs_uint64 sign = static_cast<tjs_uint64>(1) << 63;
	if((bits & sign) == 0) return static_cast<tTVInteger>(bits);
	return -1 - static_cast<tTVInteger>(~bits);
}

static inline tTVInteger TJSShiftLeft(tTVInteger value, tTVInteger count)
{
	return TJSIntegerFromBitPattern(static_cast<tjs_uint64>(value) <<
		TJSNormalizeShiftCount(count));
}

static inline tTVInteger TJSShiftArithmeticRight(
	tTVInteger value, tTVInteger count)
{
	const tjs_uint shift = TJSNormalizeShiftCount(count);
	tjs_uint64 result = static_cast<tjs_uint64>(value) >> shift;
	if(value < 0 && shift != 0)
		result |= ~static_cast<tjs_uint64>(0) << (64u - shift);
	return TJSIntegerFromBitPattern(result);
}

static inline tTVInteger TJSShiftLogicalRight(
	tTVInteger value, tTVInteger count)
{
	return TJSIntegerFromBitPattern(static_cast<tjs_uint64>(value) >>
		TJSNormalizeShiftCount(count));
}
]=])
set(MOFA_TJS_VARIANT_HEADER_BEFORE_SHIFT_HELPERS
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "${MOFA_TJS_VARIANT_SHIFT_DECLARATIONS}"
    "${MOFA_TJS_VARIANT_DEFINED_SHIFTS}"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
if(MOFA_TJS_VARIANT_HEADER_PATCHED STREQUAL
   MOFA_TJS_VARIANT_HEADER_BEFORE_SHIFT_HELPERS)
    message(FATAL_ERROR "Yuri defined shift-helper patch no longer applies")
endif()

set(MOFA_TJS_VARIANT_HEADER_BEFORE_ARITHMETIC_SHIFT
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "return (tTVInteger)(AsInteger()>>(tjs_int)rhs.AsInteger());"
    "return TJSShiftArithmeticRight(AsInteger(), rhs.AsInteger());"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
if(MOFA_TJS_VARIANT_HEADER_PATCHED STREQUAL
   MOFA_TJS_VARIANT_HEADER_BEFORE_ARITHMETIC_SHIFT)
    message(FATAL_ERROR "Yuri inline arithmetic-right-shift patch no longer applies")
endif()

set(MOFA_TJS_VARIANT_HEADER_BEFORE_LOGICAL_SHIFT
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "return (tTVInteger)((tjs_uint64)AsInteger()>> count);"
    "return TJSShiftLogicalRight(AsInteger(), count);"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
if(MOFA_TJS_VARIANT_HEADER_PATCHED STREQUAL
   MOFA_TJS_VARIANT_HEADER_BEFORE_LOGICAL_SHIFT)
    message(FATAL_ERROR "Yuri inline logical-right-shift patch no longer applies")
endif()

set(MOFA_TJS_VARIANT_HEADER_BEFORE_LEFT_SHIFT
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
string(REPLACE
    "return (tTVInteger)(AsInteger()<<(tjs_int)rhs.AsInteger());"
    "return TJSShiftLeft(AsInteger(), rhs.AsInteger());"
    MOFA_TJS_VARIANT_HEADER_PATCHED
    "${MOFA_TJS_VARIANT_HEADER_PATCHED}")
if(MOFA_TJS_VARIANT_HEADER_PATCHED STREQUAL
   MOFA_TJS_VARIANT_HEADER_BEFORE_LEFT_SHIFT)
    message(FATAL_ERROR "Yuri inline left-shift patch no longer applies")
endif()

foreach(MOFA_TJS_VARIANT_RAW_SHIFT IN ITEMS
        "AsInteger()>>(tjs_int)rhs.AsInteger()"
        "(tjs_uint64)AsInteger()>> count"
        "AsInteger()<<(tjs_int)rhs.AsInteger()")
    string(FIND "${MOFA_TJS_VARIANT_HEADER_PATCHED}"
        "${MOFA_TJS_VARIANT_RAW_SHIFT}"
        MOFA_TJS_VARIANT_RAW_SHIFT_OFFSET)
    if(NOT MOFA_TJS_VARIANT_RAW_SHIFT_OFFSET LESS 0)
        message(FATAL_ERROR
            "Yuri generated variant header retains a raw shift: ${MOFA_TJS_VARIANT_RAW_SHIFT}")
    endif()
endforeach()
if(MOFA_TJS_VARIANT_HEADER_PATCHED STREQUAL
   MOFA_TJS_VARIANT_HEADER_TEXT)
    message(FATAL_ERROR "Yuri empty variant-string header patch no longer applies")
endif()
file(CONFIGURE
    OUTPUT "${MOFA_YURI_TJS_GENERATED_DIR}/tjsVariant.h"
    CONTENT "${MOFA_TJS_VARIANT_HEADER_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

set(MOFA_TJS_VARIANT_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsVariant.cpp")
file(READ "${MOFA_TJS_VARIANT_SOURCE}" MOFA_TJS_VARIANT_TEXT)
string(REPLACE
    "\tif(vt == tvtString)\n\t\tString->ToNumber(*this);"
    "\tif(vt == tvtString)\n\t{\n\t\tif(String) String->ToNumber(*this);\n\t\telse { vt = tvtInteger; Integer = 0; }\n\t}"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_TEXT}")
string(REPLACE
    "\tif(vt==tvtString)\n\t{\n\t\tString->ToNumber(*this);\n\t\treturn;\n\t}"
    "\tif(vt==tvtString)\n\t{\n\t\tif(String) String->ToNumber(*this);\n\t\telse { vt = tvtInteger; Integer = 0; }\n\t\treturn;\n\t}"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_PATCHED}")
set(MOFA_TJS_VARIANT_BEFORE_COMPARE
    "${MOFA_TJS_VARIANT_PATCHED}")
string(REPLACE
    "\tconst tjs_char *p1 = *s1;\n\tconst tjs_char *p2 = *s2;\n\tif(!p1) p1=TJS_W(\"\");\n\tif(!p2) p2=TJS_W(\"\");"
    "\tconst tjs_char *p1 = s1 ? s1->operator const tjs_char *() : TJSNullStrPtr;\n\tconst tjs_char *p2 = s2 ? s2->operator const tjs_char *() : TJSNullStrPtr;"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_PATCHED}")
if(MOFA_TJS_VARIANT_PATCHED STREQUAL
   MOFA_TJS_VARIANT_BEFORE_COMPARE)
    message(FATAL_ERROR "Yuri empty-string comparison patch no longer applies")
endif()
set(MOFA_TJS_VARIANT_BEFORE_CONCAT
    "${MOFA_TJS_VARIANT_PATCHED}")
string(REPLACE
    "\t\tval.String = TJSAllocVariantString(*s1, *s2);"
    "\t\tval.String = TJSAllocVariantString(\n\t\t\ts1 ? s1->operator const tjs_char *() : TJSNullStrPtr,\n\t\t\ts2 ? s2->operator const tjs_char *() : TJSNullStrPtr);"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_PATCHED}")
if(MOFA_TJS_VARIANT_PATCHED STREQUAL
   MOFA_TJS_VARIANT_BEFORE_CONCAT)
    message(FATAL_ERROR "Yuri empty-string compound-concatenation patch no longer applies")
endif()

# Route compound operators through the same helpers as the inline operators.
# The VM implements ordinary binary shifts with these compound methods too, so
# this covers both bytecode paths rather than only explicit `<<=` syntax.
set(MOFA_TJS_VARIANT_BEFORE_COMPOUND_ARITHMETIC_SHIFT
    "${MOFA_TJS_VARIANT_PATCHED}")
string(REPLACE
    "\tInteger=l>>(tjs_int)rhs.AsInteger();"
    "\tInteger=TJSShiftArithmeticRight(l, rhs.AsInteger());"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_PATCHED}")
if(MOFA_TJS_VARIANT_PATCHED STREQUAL
   MOFA_TJS_VARIANT_BEFORE_COMPOUND_ARITHMETIC_SHIFT)
    message(FATAL_ERROR "Yuri compound arithmetic-right-shift patch no longer applies")
endif()

set(MOFA_TJS_VARIANT_BEFORE_COMPOUND_LOGICAL_SHIFT
    "${MOFA_TJS_VARIANT_PATCHED}")
string(REPLACE
    "\tInteger=(tjs_int64)((tjs_uint64)l>> (tjs_int)rhs);"
    "\tInteger=TJSShiftLogicalRight(l, rhs.AsInteger());"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_PATCHED}")
if(MOFA_TJS_VARIANT_PATCHED STREQUAL
   MOFA_TJS_VARIANT_BEFORE_COMPOUND_LOGICAL_SHIFT)
    message(FATAL_ERROR "Yuri compound logical-right-shift patch no longer applies")
endif()

set(MOFA_TJS_VARIANT_BEFORE_COMPOUND_LEFT_SHIFT
    "${MOFA_TJS_VARIANT_PATCHED}")
string(REPLACE
    "\tInteger=l<<(tjs_int)rhs.AsInteger();"
    "\tInteger=TJSShiftLeft(l, rhs.AsInteger());"
    MOFA_TJS_VARIANT_PATCHED "${MOFA_TJS_VARIANT_PATCHED}")
if(MOFA_TJS_VARIANT_PATCHED STREQUAL
   MOFA_TJS_VARIANT_BEFORE_COMPOUND_LEFT_SHIFT)
    message(FATAL_ERROR "Yuri compound left-shift patch no longer applies")
endif()

foreach(MOFA_TJS_VARIANT_RAW_SHIFT IN ITEMS
        "Integer=l>>(tjs_int)rhs.AsInteger();"
        "Integer=(tjs_int64)((tjs_uint64)l>> (tjs_int)rhs);"
        "Integer=l<<(tjs_int)rhs.AsInteger();")
    string(FIND "${MOFA_TJS_VARIANT_PATCHED}"
        "${MOFA_TJS_VARIANT_RAW_SHIFT}"
        MOFA_TJS_VARIANT_RAW_SHIFT_OFFSET)
    if(NOT MOFA_TJS_VARIANT_RAW_SHIFT_OFFSET LESS 0)
        message(FATAL_ERROR
            "Yuri generated variant source retains a raw shift: ${MOFA_TJS_VARIANT_RAW_SHIFT}")
    endif()
endforeach()
if(MOFA_TJS_VARIANT_PATCHED STREQUAL MOFA_TJS_VARIANT_TEXT)
    message(FATAL_ERROR "Yuri empty variant-string source patch no longer applies")
endif()
file(CONFIGURE
    OUTPUT "${MOFA_YURI_TJS_GENERATED_DIR}/tjsVariant.cpp"
    CONTENT "${MOFA_TJS_VARIANT_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

# Empty TJS strings use a null tTJSVariantString pointer.  The bytecode
# interpreter's string-property helper guarded that representation only when
# built with the obsolete CodeGuard compiler, then invoked GetLength through
# null in every normal Vita build.  This is the exact fault captured when a
# retail script evaluated an empty string's `length` property.  Preserve TJS's
# established semantics (empty length is zero, index zero yields empty) in all
# builds and in every property addressing form.
set(MOFA_TJS_INTERCODE_SOURCE
    "${MOFA_YURI_TJS_SOURCE_DIR}/tjsInterCodeExec.cpp")
file(READ "${MOFA_TJS_INTERCODE_SOURCE}" MOFA_TJS_INTERCODE_TEXT)
set(MOFA_TJS_INTERCODE_BAD_LENGTH [=[
			const tTJSVariantString * s = str->AsStringNoAddRef();
#ifdef __CODEGUARD__
			if(!s)
				*result = tTVInteger(0); // tTJSVariantString::GetLength can return zero if 'this' is NULL
			else
#endif
			*result = tTVInteger(s->GetLength());
]=])
set(MOFA_TJS_INTERCODE_FIXED_LENGTH [=[
			const tTJSVariantString * s = str->AsStringNoAddRef();
			*result = tTVInteger(s ? s->GetLength() : 0);
]=])
string(REPLACE "${MOFA_TJS_INTERCODE_BAD_LENGTH}"
    "${MOFA_TJS_INTERCODE_FIXED_LENGTH}"
    MOFA_TJS_INTERCODE_PATCHED "${MOFA_TJS_INTERCODE_TEXT}")
if(MOFA_TJS_INTERCODE_PATCHED STREQUAL MOFA_TJS_INTERCODE_TEXT)
    message(FATAL_ERROR "Yuri empty-string property-length patch no longer applies")
endif()
set(MOFA_TJS_INTERCODE_BEFORE_INDEX "${MOFA_TJS_INTERCODE_PATCHED}")
string(REPLACE
    "tjs_int len = valstr->GetLength();"
    "tjs_int len = valstr ? valstr->GetLength() : 0;"
    MOFA_TJS_INTERCODE_PATCHED "${MOFA_TJS_INTERCODE_PATCHED}")
if(MOFA_TJS_INTERCODE_PATCHED STREQUAL
   MOFA_TJS_INTERCODE_BEFORE_INDEX)
    message(FATAL_ERROR "Yuri empty-string property-index patch no longer applies")
endif()
file(CONFIGURE
    OUTPUT "${MOFA_YURI_TJS_GENERATED_DIR}/tjsInterCodeExec.cpp"
    CONTENT "${MOFA_TJS_INTERCODE_PATCHED}"
    @ONLY NEWLINE_STYLE UNIX)

file(GLOB MOFA_YURI_TJS_SOURCES CONFIGURE_DEPENDS
    "${MOFA_YURI_TJS_GENERATED_DIR}/*.cpp")

add_library(mofa-yuri-tjs STATIC
    ${MOFA_YURI_TJS_SOURCES}
    src/yuri/tjs_platform.cpp
)
set_target_properties(mofa-yuri-tjs PROPERTIES CXX_STANDARD 17)
target_include_directories(mofa-yuri-tjs PUBLIC
    src/yuri/compat
    "${MOFA_YURI_TJS_GENERATED_DIR}"
    "${MOFA_YURI_SOURCE_DIR}/src/core/environ"
    "${MOFA_YURI_SOURCE_DIR}/src/core/utils"
)
target_compile_definitions(mofa-yuri-tjs PUBLIC
    TJS_TEXT_OUT_CRLF
    USE_UNICODE_FSTRING
)
target_compile_options(mofa-yuri-tjs PRIVATE
    -include${CMAKE_CURRENT_SOURCE_DIR}/src/yuri/tjs_compat.hpp
    -Wno-deprecated-declarations
    -Wno-implicit-fallthrough
)

set(MOFA_COMMON_SOURCES
    src/common/ajpm.cpp
    src/common/bubble.cpp
    src/common/filter_heuristic.cpp
    src/common/game.cpp
    src/common/patch_manifest.cpp
    src/common/patch_repository.cpp
    src/common/phase1_filter.cpp
    src/common/pe_resources.cpp
    src/common/png.cpp
    src/common/profile.cpp
    src/engine/retail/psb.cpp
    src/common/retail_filter.cpp
    src/common/sfo.cpp
    src/common/sha256.cpp
    src/common/storage.cpp
    src/common/text_codec.cpp
    src/common/xp3_archive.cpp
    src/yuri/xp3_filter_vm.cpp
)
if(NOT VITA)
    list(APPEND MOFA_COMMON_SOURCES
        src/common/native_plugin_inventory.cpp)
endif()

add_library(mofa-common STATIC ${MOFA_COMMON_SOURCES})
target_include_directories(mofa-common PUBLIC include)
target_compile_options(mofa-common PRIVATE -Wall -Wextra -Wpedantic)
target_link_libraries(mofa-common PUBLIC mofa-yuri-tjs)

# sqlite3.dll is used by KAG environment databases for prepared statements,
# attached read-only databases, and BLOB scene state.  Build the same official
# amalgamation on host and Vita so the compatibility test and target do not
# silently exercise different SQL engines.
add_library(mofa-sqlite STATIC
    "${MOFA_SQLITE_SOURCE_DIR}/sqlite3.c")
if(VITA)
    target_sources(mofa-sqlite PRIVATE
        src/platform/vita/sqlite_posix_compat.c)
endif()
target_include_directories(mofa-sqlite BEFORE PRIVATE src/yuri/compat)
target_include_directories(mofa-sqlite PUBLIC
    "${MOFA_SQLITE_SOURCE_DIR}")
target_compile_definitions(mofa-sqlite PRIVATE
    SQLITE_THREADSAFE=0
    SQLITE_DEFAULT_MEMSTATUS=0
    SQLITE_DQS=0
    SQLITE_OMIT_LOAD_EXTENSION=1
    SQLITE_OMIT_WAL=1
    SQLITE_MAX_MMAP_SIZE=0
    SQLITE_TEMP_STORE=3)
set_target_properties(mofa-sqlite PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED ON)

if(VITA)
    # Runtime preparation includes exact pinned-patch resolution plus the
    # archive-only Phase 1 detector. It does not download content or perform
    # Phase 2 executable analysis; an inconclusive archive reports that later
    # phase explicitly instead of selecting a guessed catch-all filter.
    add_library(mofa-retail-resolver STATIC
        src/common/filter_heuristic.cpp
        src/common/game.cpp
        src/common/patch_manifest.cpp
        src/common/phase1_filter.cpp
        src/common/pe_resources.cpp
        src/common/sha256.cpp
        src/common/xp3_archive.cpp
        src/yuri/xp3_filter_vm.cpp
    )
    target_include_directories(mofa-retail-resolver PUBLIC include)
    target_compile_options(mofa-retail-resolver PRIVATE
        -Wall -Wextra -Wpedantic)
    target_link_libraries(mofa-retail-resolver PUBLIC
        mofa-yuri-tjs
        z)

    include(FetchContent)
    FetchContent_Declare(mofa_retail_patches
        GIT_REPOSITORY https://github.com/zeas2/Kirikiroid2_patch.git
        GIT_TAG 0af1bafdb7a031e4c74fd4b320b6a0a1ec03b300
        GIT_SHALLOW TRUE
        GIT_PROGRESS FALSE
    )
    FetchContent_MakeAvailable(mofa_retail_patches)
    set(MOFA_PATCH_SOURCE_DIR
        "${mofa_retail_patches_SOURCE_DIR}"
        CACHE INTERNAL "Pinned Kirikiroid2 retail patch snapshot")

    # The sample's KAG scripts use the complete Kirikiri 2 extrans transition
    # set, not just the three handlers built into the core. Fetch the original
    # plugin sources at a fixed revision; YuriBackend.cmake adapts only the
    # DLL/import boundary and the x86-only ripple kernels for static ARM use.
    FetchContent_Declare(mofa_extrans
        GIT_REPOSITORY https://github.com/krkrz/SamplePlugin.git
        GIT_TAG d94e312065f7298a970b58bfe73de48e52aef9ee
        GIT_SHALLOW TRUE
        GIT_PROGRESS FALSE
    )
    FetchContent_MakeAvailable(mofa_extrans)
    set(MOFA_EXTRANS_SOURCE_DIR
        "${mofa_extrans_SOURCE_DIR}/extrans"
        CACHE INTERNAL "Pinned Kirikiri extrans source directory")

    FetchContent_Declare(mofa_layerexsave
        GIT_REPOSITORY https://github.com/wtnbgo/layerExSave.git
        GIT_TAG b53d5bf6616fcaf31c8a095ff414b17e4274445e
        # The pinned revision is no longer the default-branch head upstream and
        # a shallow clone only carries that head, which makes the follow-up
        # checkout fail with "reference is not a tree". Fetch full history.
        GIT_SHALLOW FALSE
        GIT_PROGRESS FALSE
        SOURCE_SUBDIR mofa-fetch-only
    )
    FetchContent_MakeAvailable(mofa_layerexsave)
    set(MOFA_LAYEREXSAVE_SOURCE_DIR
        "${mofa_layerexsave_SOURCE_DIR}"
        CACHE INTERNAL "Pinned Kirikiri layerExSave source directory")

    # LayerExImage is a common KAGEX dependency which extends Layer with the
    # pixel effects used by brightness, colour and blur animation paths. Keep
    # the original implementation pinned and adapt it into Yuri's static
    # internal-plugin registry in the Vita backend.
    FetchContent_Declare(mofa_layereximage
        GIT_REPOSITORY https://github.com/wtnbgo/layerExImage.git
        GIT_TAG 98c53ec33f5d99cea007eb5397fffd1321f34343
        # Same as layerExSave: the pinned revision is no longer reachable from
        # a single-branch shallow clone.
        GIT_SHALLOW FALSE
        GIT_PROGRESS FALSE
        SOURCE_SUBDIR mofa-fetch-only
    )
    FetchContent_MakeAvailable(mofa_layereximage)
    set(MOFA_LAYEREXIMAGE_SOURCE_DIR
        "${mofa_layereximage_SOURCE_DIR}"
        CACHE INTERNAL "Pinned Kirikiri layerExImage source directory")

    # Squirrel is a real KAG extension, not a Windows-only optional cosmetic
    # plugin: KAGEX titles use its fork/drive/wait API for scripted motion and
    # effects.  Reuse the upstream Kirikiri implementation and adapt only its
    # TJS/storage boundary in YuriBackend.cmake.  Pin the exact source because
    # the VM bytecode and TJS bridge are part of the compatibility contract.
    FetchContent_Declare(mofa_squirrel
        GIT_REPOSITORY https://github.com/krkrz/krkrz.git
        GIT_TAG 49c4d53506edecb824cd7b2cff8d32959b1f1b70
        # krkrz has moved past the pinned revision, so the clone must carry the
        # full history for the checked-out source to resolve.
        GIT_SHALLOW FALSE
        GIT_PROGRESS FALSE
    )
    FetchContent_MakeAvailable(mofa_squirrel)
    set(MOFA_SQUIRREL_SOURCE_DIR
        "${mofa_squirrel_SOURCE_DIR}/src/plugins/win32/squirrel"
        CACHE INTERNAL "Pinned Kirikiri Squirrel source directory")

    # Some KAGEX titles use psd.dll for their layered PSD scene
    # backgrounds. Reuse the maintained Kirikiri PSD parser/plugin source;
    # YuriBackend.cmake only adapts its storage/registration boundary and
    # keeps the parser itself authoritative.
    FetchContent_Declare(mofa_psdfile
        GIT_REPOSITORY https://github.com/2468785842/krkr2.git
        GIT_TAG dca7264572a63e753c1b07ca7053513d1751ce70
        GIT_SHALLOW TRUE
        GIT_PROGRESS FALSE
        SOURCE_SUBDIR mofa-fetch-only
    )
    FetchContent_MakeAvailable(mofa_psdfile)
    set(MOFA_PSDFILE_SOURCE_DIR
        "${mofa_psdfile_SOURCE_DIR}/cpp/plugins/psdfile"
        CACHE INTERNAL "Pinned Kirikiri PSD plugin source directory")

    target_compile_definitions(mofa-yuri-tjs PUBLIC
        MOFA_USE_ONIGURUMA=1)
    target_link_libraries(mofa-yuri-tjs PUBLIC onig)
    include("${VITASDK}/share/vita.cmake")
    include(cmake/YuriBackend.cmake)
    mofa_add_yuri_backend_targets()

    # mofa-common reaches the retail patch repository over HTTP through
    # libcurl. The host branch links CURL::libcurl onto that archive, but the
    # Vita backend never did, so the final link failed with undefined curl_*
    # references. Attach VitaSDK's imported target the same way: it lands right
    # after libmofa-common.a and brings mbedTLS, zlib and friends with it.
    if(TARGET mofa-common)
        find_package(CURL REQUIRED)
        target_link_libraries(mofa-common PUBLIC CURL::libcurl)
    endif()
else()
    # A matching header/library pair matters here: Ruby's private Onigmo
    # header is not ABI-compatible with a distribution libonig. Pinning the
    # small host dependency also makes regex-enabled script probes repeatable.
    target_compile_definitions(mofa-yuri-tjs PUBLIC
        MOFA_USE_ONIGURUMA=1)
    target_link_libraries(mofa-yuri-tjs PUBLIC onig)
    find_package(CURL REQUIRED)
    find_package(OpenSSL REQUIRED)
    find_package(PNG REQUIRED)
    find_package(SQLite3 REQUIRED)
    find_package(ZLIB REQUIRED)
    target_link_libraries(mofa-common PUBLIC CURL::libcurl PNG::PNG ZLIB::ZLIB)

    add_executable(mofa-tool src/tool/main.cpp)
    target_link_libraries(mofa-tool PRIVATE mofa-common)

    if(MOFA_BUILD_TESTS)
        enable_testing()
        add_executable(mofa-tests tests/test_main.cpp)
        target_link_libraries(mofa-tests PRIVATE mofa-common)
        target_compile_definitions(mofa-tests PRIVATE
            MOFA_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}")
        add_test(NAME mofa-tests COMMAND mofa-tests)

        # A retail compatibility result must exercise the same generated XP3
        # filter and TJS compiler as the Vita backend.  This executable audits
        # one immutable game fingerprint at a time: every TJS unit and every
        # KAG inline-script block is compiled, every supported image/audio
        # payload is signature-checked, and boot-reachable plugin requests plus
        # unsupported runtime formats are rejected from a shared inventory. The
        # concrete local corpus is registered below when its directories are
        # available; CI can provide the same paths through the cache list.
        add_executable(mofa-retail-compatibility
            tests/test_retail_compatibility.cpp)
        target_link_libraries(mofa-retail-compatibility PRIVATE
            mofa-common)
        add_test(NAME mofa-retail-compatibility-contracts
            COMMAND mofa-retail-compatibility --self-test)
        set_tests_properties(mofa-retail-compatibility-contracts
            PROPERTIES LABELS "retail;compatibility")
        add_executable(mofa-retail-armv7-runtime
            tests/retail_armv7_runtime.cpp)
        target_include_directories(mofa-retail-armv7-runtime PRIVATE include)
        target_link_libraries(mofa-retail-armv7-runtime PRIVATE
            mofa-yuri-tjs)
        set(MOFA_RETAIL_COMPATIBILITY_MANIFEST
            "" CACHE FILEPATH
            "Optional local retail compatibility manifest")
        set(MOFA_RETAIL_HARDWARE_EVIDENCE
            "" CACHE FILEPATH
            "Optional local hardware compatibility evidence")
        if(EXISTS "${MOFA_RETAIL_COMPATIBILITY_MANIFEST}")
            add_custom_target(mofa-final-compatibility-gate
                COMMAND "${CMAKE_COMMAND}" -E env
                    "MOFA_RETAIL_RUNNER=$<TARGET_FILE:mofa-retail-compatibility>"
                    "MOFA_RETAIL_MANIFEST=${MOFA_RETAIL_COMPATIBILITY_MANIFEST}"
                    "${CMAKE_CURRENT_SOURCE_DIR}/scripts/run-retail-compatibility.sh"
                    --require-all
                DEPENDS mofa-retail-compatibility
                USES_TERMINAL
                COMMENT "Running fail-closed retail compatibility gate")

            file(STRINGS "${MOFA_RETAIL_COMPATIBILITY_MANIFEST}"
                mofa_retail_compatibility_rows ENCODING UTF-8)
            foreach(mofa_retail_row IN LISTS
                    mofa_retail_compatibility_rows)
                if(mofa_retail_row MATCHES "^#" OR
                   mofa_retail_row STREQUAL "")
                    continue()
                endif()
                string(REPLACE "|" ";" mofa_retail_fields
                    "${mofa_retail_row}")
                list(LENGTH mofa_retail_fields mofa_retail_field_count)
                if(NOT mofa_retail_field_count EQUAL 8)
                    message(FATAL_ERROR
                        "Malformed retail compatibility row (${mofa_retail_field_count} fields): ${mofa_retail_row}")
                endif()
                list(GET mofa_retail_fields 0 mofa_retail_id)
                list(GET mofa_retail_fields 1 mofa_retail_path)
                list(GET mofa_retail_fields 2 mofa_retail_fingerprint)
                list(GET mofa_retail_fields 3 mofa_retail_expectation)
                list(GET mofa_retail_fields 4 mofa_retail_rule)
                list(GET mofa_retail_fields 5 mofa_retail_recognized)
                list(GET mofa_retail_fields 6 mofa_retail_samples)
                list(GET mofa_retail_fields 7 mofa_retail_diagnostic)
                if(NOT EXISTS "${mofa_retail_path}")
                    continue()
                endif()
                if(mofa_retail_expectation STREQUAL "phase1")
                    set(mofa_retail_test_name
                        "mofa-retail-compatibility-${mofa_retail_id}")
                    add_test(NAME "${mofa_retail_test_name}"
                        COMMAND mofa-retail-compatibility --game
                            "${mofa_retail_path}"
                            "${mofa_retail_fingerprint}"
                            "${mofa_retail_rule}"
                            "${mofa_retail_recognized}"
                            "${mofa_retail_samples}")
                elseif(mofa_retail_expectation STREQUAL "runtime_blocked")
                    set(mofa_retail_test_name
                        "mofa-retail-runtime-blocked-${mofa_retail_id}")
                    add_test(NAME "${mofa_retail_test_name}"
                        COMMAND mofa-retail-compatibility
                            --expect-runtime-blocker
                            "${mofa_retail_path}"
                            "${mofa_retail_fingerprint}"
                            "${mofa_retail_rule}"
                            "${mofa_retail_recognized}"
                            "${mofa_retail_samples}"
                            "${mofa_retail_diagnostic}")
                elseif(mofa_retail_expectation STREQUAL "hardware_blocked")
                    # The host audit must keep passing; the actual blocker is
                    # recorded separately and is only observable on the device.
                    set(mofa_retail_test_name
                        "mofa-retail-hardware-blocked-${mofa_retail_id}")
                    add_test(NAME "${mofa_retail_test_name}"
                        COMMAND mofa-retail-compatibility --game
                            "${mofa_retail_path}"
                            "${mofa_retail_fingerprint}"
                            "${mofa_retail_rule}"
                            "${mofa_retail_recognized}"
                            "${mofa_retail_samples}")
                elseif(mofa_retail_expectation STREQUAL "phase2")
                    set(mofa_retail_test_name
                        "mofa-retail-phase2-${mofa_retail_id}")
                    add_test(NAME "${mofa_retail_test_name}"
                        COMMAND mofa-retail-compatibility --expect-phase2
                            "${mofa_retail_path}"
                            "${mofa_retail_fingerprint}")
                else()
                    message(FATAL_ERROR
                        "Unknown retail expectation: ${mofa_retail_expectation}")
                endif()
                set_tests_properties(
                    "${mofa_retail_test_name}"
                    PROPERTIES TIMEOUT 900 LABELS "retail;compatibility")
            endforeach()
        endif()

        # Exercise the exact generated ObjectList overlay used by the Vita
        # backend.  The test counts backing-array allocations across stable
        # safe locks and then verifies that an in-lock removal still takes a
        # snapshot and is compacted at the next traversal boundary.
        add_executable(mofa-yuri-object-list-test
            tests/test_yuri_object_list.cpp)
        target_include_directories(mofa-yuri-object-list-test BEFORE PRIVATE
            "${CMAKE_CURRENT_BINARY_DIR}/generated/yuri"
            "${MOFA_YURI_TJS_GENERATED_DIR}"
            "${MOFA_YURI_SOURCE_DIR}/src/core/utils")
        add_test(NAME mofa-yuri-object-list
            COMMAND mofa-yuri-object-list-test)

        # Yuri's timers post zero-argument TJS events. Exercise the exact
        # argument-storage and delivery helpers used by the generated Vita
        # EventIntf overlay: the empty path must pass a null argv without an
        # allocation, while nonempty ordering and exception cleanup stay intact.
        add_executable(mofa-yuri-event-arguments-test
            tests/test_yuri_event_arguments.cpp)
        target_include_directories(mofa-yuri-event-arguments-test PRIVATE
            include)
        add_test(NAME mofa-yuri-event-arguments
            COMMAND mofa-yuri-event-arguments-test)

        # Retail KAG scripts request verbose parser logging unconditionally.
        # Pin the Vita policy that retains scenario-level diagnostics while
        # suppressing only the synchronous per-tag/macro/call-stack flood.
        add_executable(mofa-kag-log-policy-test
            tests/test_kag_log_policy.cpp)
        target_include_directories(mofa-kag-log-policy-test PRIVATE
            include)
        add_test(NAME mofa-kag-log-policy
            COMMAND mofa-kag-log-policy-test)

        # Squirrel is compiled for Vita with a UTF-16 char16_t bridge rather
        # than the host's 32-bit wchar_t ABI. Exercise the exact bridge used
        # by the generated VM: string/search/token APIs, UTF-8 path conversion,
        # numeric parsing, and the bounded formatter used by printFunc.
        add_executable(mofa-yuri-squirrel-compat-test
            tests/test_yuri_squirrel_compat.cpp)
        target_include_directories(mofa-yuri-squirrel-compat-test PRIVATE
            src/engine/retail)
        add_test(NAME mofa-yuri-squirrel-compat
            COMMAND mofa-yuri-squirrel-compat-test)

        # Large retail [iscript] blocks must be assembled with one bounded
        # allocation before the TJS callback. This pins byte parity and guards
        # against Yuri's former append-per-line allocation growth.
        add_executable(mofa-kag-inline-script-test
            tests/test_kag_inline_script.cpp)
        target_include_directories(mofa-kag-inline-script-test PRIVATE
            include)
        add_test(NAME mofa-kag-inline-script
            COMMAND mofa-kag-inline-script-test)

        # sigcheck.dll validates loose XP3 files with SHA-256/RSA-PSS while
        # scripts continue booting. Pin its exact envelope, streaming verifier,
        # tamper rejection and (when installed) the retail data.xp3 signature.
        add_executable(mofa-rsa-pss-signature-test
            tests/test_rsa_pss_signature.cpp
            src/engine/retail/rsa_pss_signature.cpp)
        target_include_directories(mofa-rsa-pss-signature-test PRIVATE
            include)
        target_link_libraries(mofa-rsa-pss-signature-test PRIVATE
            OpenSSL::Crypto)
        add_test(NAME mofa-rsa-pss-signature
            COMMAND mofa-rsa-pss-signature-test)

        # psbfile.dll consumes both ordinary PSB assets and MDF/zlib scene
        # blobs returned by SQLite. Exercise a synthetic boundary fixture plus
        # explicitly configured external animation and scene databases.
        add_executable(mofa-psb-test tests/test_psb.cpp)
        target_link_libraries(mofa-psb-test PRIVATE
            mofa-common SQLite::SQLite3)
        add_test(NAME mofa-psb COMMAND mofa-psb-test)

        # AJPM is AlphaMovie.dll's undocumented container. Synthetic fixtures
        # pin the structural contract and the fail-closed paths; an explicitly
        # configured local archive can additionally walk every shipped frame.
        add_executable(mofa-ajpm-test tests/test_ajpm.cpp)
        target_link_libraries(mofa-ajpm-test PRIVATE
            mofa-common ZLIB::ZLIB)
        add_test(NAME mofa-ajpm COMMAND mofa-ajpm-test)

        # Compile the exact sqlite3.dll adapter used by Vita against Yuri's
        # real TJS VM.  This pins the official KiriKiri statement API rather
        # than treating a successful SQLite C-library build as compatibility.
        add_executable(mofa-yuri-sqlite-module-test
            tests/test_yuri_sqlite_module.cpp
            src/engine/retail/yuri_sqlite_module.cpp
            "${MOFA_YURI_SOURCE_DIR}/src/plugins/ncbind/ncbind.cpp")
        target_include_directories(mofa-yuri-sqlite-module-test BEFORE PRIVATE
            include
            src/yuri/compat
            "${MOFA_YURI_SOURCE_DIR}/src/core/base"
            "${MOFA_YURI_SOURCE_DIR}/src/core/base/win32"
            "${MOFA_YURI_SOURCE_DIR}/src/core/environ"
            "${MOFA_YURI_SOURCE_DIR}/src/core/msg"
            "${MOFA_YURI_SOURCE_DIR}/src/core/msg/win32"
            "${MOFA_YURI_SOURCE_DIR}/src/core/tjs2"
            "${MOFA_YURI_SOURCE_DIR}/src/core/utils"
            "${MOFA_YURI_SOURCE_DIR}/src/core/utils/win32"
            "${MOFA_YURI_SOURCE_DIR}/src/plugins"
            "${MOFA_YURI_SOURCE_DIR}/src/plugins/ncbind")
        target_compile_options(mofa-yuri-sqlite-module-test PRIVATE
            -include${CMAKE_CURRENT_SOURCE_DIR}/src/yuri/tjs_compat.hpp
            -Wno-deprecated-declarations)
        target_link_libraries(mofa-yuri-sqlite-module-test PRIVATE
            mofa-yuri-tjs mofa-sqlite)
        add_test(NAME mofa-yuri-sqlite-module
            COMMAND mofa-yuri-sqlite-module-test)

        # When supplied, the executable manifest and physical-Vita receipts
        # must agree. A blocked hardware receipt always outranks a host audit.
        if(EXISTS "${MOFA_RETAIL_COMPATIBILITY_MANIFEST}" AND
           EXISTS "${MOFA_RETAIL_HARDWARE_EVIDENCE}")
            add_test(NAME mofa-compatibility-claims
                COMMAND "${CMAKE_COMMAND}"
                    -DMANIFEST=${MOFA_RETAIL_COMPATIBILITY_MANIFEST}
                    -DEVIDENCE=${MOFA_RETAIL_HARDWARE_EVIDENCE}
                    -P "${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyCompatibilityClaims.cmake")
            set_tests_properties(mofa-compatibility-claims
                PROPERTIES LABELS "compatibility")
        endif()

        # Vita seals plugin loading, so Plugins.link() succeeds only when a
        # module is present in the ncbAutoRegister registry. Compiling the
        # fallback source and finding its marker string in the ELF proves
        # neither registration nor that the surface literal executes. Drive the
        # real registry and the real TJS VM instead.
        add_executable(mofa-yuri-plugin-registry-test
            tests/test_yuri_plugin_registry.cpp
            src/engine/retail/yuri_motionplayer_module.cpp
            src/engine/retail/yuri_layerexdraw_module.cpp
            "${MOFA_KRKR2_NEXT_SOURCE_DIR}/cpp/plugins/scriptsEx.cpp"
            src/engine/retail/yuri_scriptsex_module.cpp
            "${MOFA_YURI_SOURCE_DIR}/src/core/utils/md5.c"
            "${MOFA_YURI_SOURCE_DIR}/src/plugins/ncbind/ncbind.cpp")
        target_include_directories(mofa-yuri-plugin-registry-test
            BEFORE PRIVATE
            include
            src/yuri/compat
            "${MOFA_YURI_SOURCE_DIR}/src/core/base"
            "${MOFA_YURI_SOURCE_DIR}/src/core/base/win32"
            "${MOFA_YURI_SOURCE_DIR}/src/core/environ"
            "${MOFA_YURI_SOURCE_DIR}/src/core/msg"
            "${MOFA_YURI_SOURCE_DIR}/src/core/msg/win32"
            "${MOFA_YURI_SOURCE_DIR}/src/core/tjs2"
            "${MOFA_YURI_SOURCE_DIR}/src/core/utils"
            "${MOFA_YURI_SOURCE_DIR}/src/core/utils/win32"
            "${MOFA_YURI_SOURCE_DIR}/src/plugins"
            "${MOFA_YURI_SOURCE_DIR}/src/plugins/ncbind")
        target_compile_options(mofa-yuri-plugin-registry-test PRIVATE
            $<$<COMPILE_LANGUAGE:CXX>:-include${CMAKE_CURRENT_SOURCE_DIR}/src/yuri/tjs_compat.hpp>
            -Wno-deprecated-declarations)
        target_link_libraries(mofa-yuri-plugin-registry-test PRIVATE
            mofa-yuri-tjs)
        add_test(NAME mofa-yuri-plugin-registry
            COMMAND mofa-yuri-plugin-registry-test)

        # saveStruct emits quoted dictionary strings one code unit at a time.
        # The Vita plugin aggregates those writes into a fixed buffer; pin the
        # exact output and syscall-count reduction at the recovered datasu size.
        add_executable(mofa-buffered-units-test
            tests/test_buffered_units.cpp)
        target_include_directories(mofa-buffered-units-test PRIVATE include)
        add_test(NAME mofa-buffered-units
            COMMAND mofa-buffered-units-test)

        # The Vita compositor keeps small render primitives on one task and
        # admits only large work to its persistent worker pool. Verify that
        # the serial boundary invokes the original closure without an owning
        # std::function allocation, while the parallel adapter still owns and
        # executes every row partition.
        add_executable(mofa-render-task-dispatch-test
            tests/test_render_task_dispatch.cpp)
        target_include_directories(mofa-render-task-dispatch-test PRIVATE
            include)
        add_test(NAME mofa-render-task-dispatch
            COMMAND mofa-render-task-dispatch-test)

        # The Vita build replaces Android OpenCV with a compact compatibility
        # header. Pin its resize, box-filter, affine, and perspective pixel
        # contracts (including pitched external destinations and row-split
        # dispatch) to OpenCV 4.7 oracle outputs.
        add_executable(mofa-yuri-opencv-compat-test
            tests/test_yuri_opencv_compat.cpp)
        target_include_directories(mofa-yuri-opencv-compat-test PRIVATE
            src/yuri/compat)
        add_test(NAME mofa-yuri-opencv-compat
            COMMAND mofa-yuri-opencv-compat-test)

        # msgothic.ttc is user-supplied and must never be packaged. When it is
        # present in a personal checkout, compare the bounded cached stream and
        # prepared-glyph path against FreeType's ordinary two-load path using
        # face zero, including exact raster bytes and metrics.
        find_package(Freetype QUIET)
        if(Freetype_FOUND AND EXISTS "${MOFA_MS_GOTHIC_TTC}")
            add_executable(mofa-ms-gothic-golden
                tests/test_ms_gothic_freetype.cpp)
            target_include_directories(mofa-ms-gothic-golden PRIVATE
                include)
            target_link_libraries(mofa-ms-gothic-golden PRIVATE
                Freetype::Freetype)
            add_test(NAME mofa-ms-gothic-golden
                COMMAND mofa-ms-gothic-golden
                    "${MOFA_MS_GOTHIC_TTC}")
        endif()
        add_test(NAME mofa-vita-trace-contract
            COMMAND "${CMAKE_COMMAND}"
                -DTRACE=${CMAKE_CURRENT_SOURCE_DIR}/tests/fixtures/vita_boot_success.txt
                -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyHardwareBootTrace.cmake)
        add_test(NAME mofa-vita-trace-rejects-partial
            COMMAND "${CMAKE_COMMAND}"
                -DTRACE=${CMAKE_CURRENT_SOURCE_DIR}/tests/fixtures/vita_boot_partial.txt
                -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyHardwareBootTrace.cmake)
        set_tests_properties(mofa-vita-trace-rejects-partial PROPERTIES
            WILL_FAIL TRUE)

        # Exercise the proprietary-font packaging boundary without copying the
        # user's real font. Both fixtures live only in the host build tree.
        set(mofa_ttc_guard_clean_dir
            "${CMAKE_CURRENT_BINARY_DIR}/tests/ttc-guard-clean")
        set(mofa_ttc_guard_bad_dir
            "${CMAKE_CURRENT_BINARY_DIR}/tests/ttc-guard-bad")
        file(MAKE_DIRECTORY
            "${mofa_ttc_guard_clean_dir}"
            "${mofa_ttc_guard_bad_dir}")
        file(WRITE "${mofa_ttc_guard_clean_dir}/payload.txt"
            "allowed payload\n")
        file(WRITE "${mofa_ttc_guard_bad_dir}/font.TtC"
            "synthetic test marker, not a font\n")
        add_test(NAME mofa-package-allows-non-ttc
            COMMAND "${CMAKE_COMMAND}"
                -DTTC_SCAN_ROOT=${mofa_ttc_guard_clean_dir}
                -DTTC_SCAN_CONTEXT=Test-package
                -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyNoTtcPayload.cmake)
        add_test(NAME mofa-package-rejects-ttc
            COMMAND "${CMAKE_COMMAND}"
                -DTTC_SCAN_ROOT=${mofa_ttc_guard_bad_dir}
                -DTTC_SCAN_CONTEXT=Test-package
                -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyNoTtcPayload.cmake)
        set_tests_properties(mofa-package-rejects-ttc PROPERTIES
            WILL_FAIL TRUE)

        set(MOFA_RETAIL_TEST_GAME "" CACHE PATH
            "Optional real retail game directory for integration tests")
        set(MOFA_RETAIL_TEST_PATCH_SNAPSHOT "" CACHE PATH
            "Optional extracted Kirikiroid2 patch snapshot for integration tests")
        set(MOFA_RETAIL_TEST_FILTER "" CACHE FILEPATH
            "Optional extraction filter for retail audio integration tests")
        if(MOFA_RETAIL_TEST_GAME AND
           MOFA_RETAIL_TEST_PATCH_SNAPSHOT)
            add_test(NAME mofa-retail-sample
                COMMAND mofa-tests --retail-sample
                    "${MOFA_RETAIL_TEST_GAME}"
                    "${MOFA_RETAIL_TEST_PATCH_SNAPSHOT}")
            set_tests_properties(mofa-retail-sample PROPERTIES
                TIMEOUT 300
                LABELS "retail;integration")

            find_program(MOFA_FFMPEG_EXECUTABLE ffmpeg)
            if(MOFA_FFMPEG_EXECUTABLE AND
               EXISTS "${MOFA_RETAIL_TEST_FILTER}")
                add_test(NAME mofa-retail-audio
                    COMMAND "${CMAKE_COMMAND}"
                        -DTOOL=$<TARGET_FILE:mofa-tool>
                        -DGAME_DIR=${MOFA_RETAIL_TEST_GAME}
                        -DFILTER=${MOFA_RETAIL_TEST_FILTER}
                        -DFFMPEG=${MOFA_FFMPEG_EXECUTABLE}
                        -DWORK_DIR=${CMAKE_CURRENT_BINARY_DIR}/retail-audio-test
                        -P "${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyRetailAudio.cmake")
                set_tests_properties(mofa-retail-audio PROPERTIES
                    TIMEOUT 120
                    LABELS "retail;integration;audio"
                    RUN_SERIAL TRUE)
            endif()
        endif()
    endif()
endif()
