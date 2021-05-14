initSidebarItems({"fn":[["as_boolean_array","Force downcast ArrayRef to  BooleanArray"],["as_dictionary_array","Force downcast ArrayRef to DictionaryArray"],["as_generic_list_array","Force downcast ArrayRef to GenericListArray"],["as_large_list_array","Force downcast ArrayRef to LargeListArray"],["as_largestring_array","Force downcast ArrayRef to  LargeStringArray"],["as_list_array","Force downcast ArrayRef to ListArray"],["as_null_array","Force downcast ArrayRef to  NullArray"],["as_primitive_array","Force downcast ArrayRef to PrimitiveArray"],["as_string_array","Force downcast ArrayRef to  StringArray"],["as_struct_array","Force downcast ArrayRef to  StructArray"],["build_compare","returns a comparison function that compares two values at two different positions between the two arrays. The arrays’ types must be equal."],["make_array","Constructs an array using the input `data`. Returns a reference-counted `Array` instance."],["make_array_from_raw","Creates a new array from two FFI pointers. Used to import arrays from the C Data Interface"],["new_empty_array","Creates a new empty array"],["new_null_array","Creates a new array of `data_type` of length `length` filled entirely of `NULL` values"]],"struct":[["ArrayData","An generic representation of Arrow array data which encapsulates common attributes and operations for Arrow array. Specific operations for different arrays types (e.g., primitive, list, struct) are implemented in `Array`."],["ArrayDataBuilder","Builder for `ArrayData` type"],["BooleanArray","Array of bools"],["BooleanBufferBuilder",""],["BooleanBuilder","Array builder for fixed-width primitive types"],["BooleanIter","an iterator that returns Some(bool) or None."],["BufferBuilder","Builder for creating a `Buffer` object."],["DecimalArray","A type of `DecimalArray` whose elements are binaries."],["DecimalBuilder",""],["DictionaryArray","A dictionary array where each element is a single value indexed by an integer key. This is mostly used to represent strings or a limited set of primitive types as integers, for example when doing NLP analysis or representing chromosomes by name."],["FixedSizeBinaryArray","A type of `FixedSizeListArray` whose elements are binaries."],["FixedSizeBinaryBuilder",""],["FixedSizeListArray","A list array where each element is a fixed-size sequence of values with the same type whose maximum length is represented by a i32."],["FixedSizeListBuilder","Array builder for `ListArray`"],["GenericBinaryArray",""],["GenericBinaryIter","an iterator that returns `Some(&[u8])` or `None`, for binary arrays"],["GenericListArray",""],["GenericListArrayIter",""],["GenericStringArray","Generic struct for [Large]StringArray"],["GenericStringBuilder",""],["GenericStringIter","an iterator that returns `Some(&str)` or `None`, for string arrays"],["MutableArrayData","Struct to efficiently and interactively create an [ArrayData] from an existing [ArrayData] by copying chunks. The main use case of this struct is to perform unary operations to arrays of arbitrary types, such as `filter` and `take`."],["NullArray","An Array where all elements are nulls"],["PrimitiveArray","Array whose elements are of primitive types."],["PrimitiveBuilder","Array builder for fixed-width primitive types"],["PrimitiveDictionaryBuilder","Array builder for `DictionaryArray`. For example to map a set of byte indices to f32 values. Note that the use of a `HashMap` here will not scale to very large arrays or result in an ordered dictionary."],["PrimitiveIter","an iterator that returns Some(T) or None, that can be used on any PrimitiveArray"],["StringDictionaryBuilder","Array builder for `DictionaryArray` that stores Strings. For example to map a set of byte indices to String values. Note that the use of a `HashMap` here will not scale to very large arrays or result in an ordered dictionary."],["StructArray","A nested array type where each child (called field) is represented by a separate array."],["StructBuilder","Array builder for Struct types."],["UnionArray","An Array that can represent slots of varying types."],["UnionBuilder","Builder type for creating a new `UnionArray`."]],"trait":[["Array","Trait for dealing with different types of array at runtime when the type of the array is not known in advance."],["ArrayBuilder","Trait for dealing with different array builders at runtime"],["BinaryOffsetSizeTrait","Like OffsetSizeTrait, but specialized for Binary"],["JsonEqual","Trait for comparing arrow array with json array"],["OffsetSizeTrait","trait declaring an offset size, relevant for i32 vs i64 array types."],["StringOffsetSizeTrait","Like OffsetSizeTrait, but specialized for Strings"]],"type":[["ArrayDataRef",""],["ArrayRef","A reference-counted reference to a generic `Array`."],["BinaryArray","An array where each element is a byte whose maximum length is represented by a i32."],["BinaryBuilder",""],["Date32Array",""],["Date32BufferBuilder",""],["Date32Builder",""],["Date64Array",""],["Date64BufferBuilder",""],["Date64Builder",""],["DurationMicrosecondArray",""],["DurationMicrosecondBufferBuilder",""],["DurationMicrosecondBuilder",""],["DurationMillisecondArray",""],["DurationMillisecondBufferBuilder",""],["DurationMillisecondBuilder",""],["DurationNanosecondArray",""],["DurationNanosecondBufferBuilder",""],["DurationNanosecondBuilder",""],["DurationSecondArray",""],["DurationSecondBufferBuilder",""],["DurationSecondBuilder",""],["DynComparator","Compare the values at two arbitrary indices in two arrays."],["Float32Array",""],["Float32BufferBuilder",""],["Float32Builder",""],["Float64Array",""],["Float64BufferBuilder",""],["Float64Builder",""],["Int16Array",""],["Int16BufferBuilder",""],["Int16Builder",""],["Int16DictionaryArray",""],["Int32Array",""],["Int32BufferBuilder",""],["Int32Builder",""],["Int32DictionaryArray",""],["Int64Array",""],["Int64BufferBuilder",""],["Int64Builder",""],["Int64DictionaryArray",""],["Int8Array",""],["Int8BufferBuilder",""],["Int8Builder",""],["Int8DictionaryArray",""],["IntervalDayTimeArray",""],["IntervalDayTimeBufferBuilder",""],["IntervalDayTimeBuilder",""],["IntervalYearMonthArray",""],["IntervalYearMonthBufferBuilder",""],["IntervalYearMonthBuilder",""],["LargeBinaryArray","An array where each element is a byte whose maximum length is represented by a i64."],["LargeBinaryBuilder",""],["LargeListArray","A list array where each element is a variable-sized sequence of values with the same type whose memory offsets between elements are represented by a i64."],["LargeListBuilder",""],["LargeStringArray","An array where each element is a variable-sized sequence of bytes representing a string whose maximum length (in bytes) is represented by a i64."],["LargeStringBuilder",""],["ListArray","A list array where each element is a variable-sized sequence of values with the same type whose memory offsets between elements are represented by a i32."],["ListBuilder",""],["StringArray","An array where each element is a variable-sized sequence of bytes representing a string whose maximum length (in bytes) is represented by a i32."],["StringBuilder",""],["Time32MillisecondArray",""],["Time32MillisecondBufferBuilder",""],["Time32MillisecondBuilder",""],["Time32SecondArray",""],["Time32SecondBufferBuilder",""],["Time32SecondBuilder",""],["Time64MicrosecondArray",""],["Time64MicrosecondBufferBuilder",""],["Time64MicrosecondBuilder",""],["Time64NanosecondArray",""],["Time64NanosecondBufferBuilder",""],["Time64NanosecondBuilder",""],["TimestampMicrosecondArray",""],["TimestampMicrosecondBufferBuilder",""],["TimestampMicrosecondBuilder",""],["TimestampMillisecondArray",""],["TimestampMillisecondBufferBuilder",""],["TimestampMillisecondBuilder",""],["TimestampNanosecondArray",""],["TimestampNanosecondBufferBuilder",""],["TimestampNanosecondBuilder",""],["TimestampSecondArray",""],["TimestampSecondBufferBuilder",""],["TimestampSecondBuilder",""],["UInt16Array",""],["UInt16BufferBuilder",""],["UInt16Builder",""],["UInt16DictionaryArray",""],["UInt32Array",""],["UInt32BufferBuilder",""],["UInt32Builder",""],["UInt32DictionaryArray",""],["UInt64Array",""],["UInt64BufferBuilder",""],["UInt64Builder",""],["UInt64DictionaryArray",""],["UInt8Array",""],["UInt8BufferBuilder",""],["UInt8Builder",""],["UInt8DictionaryArray",""]]});