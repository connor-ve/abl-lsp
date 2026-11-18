/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
CLASS System.Convert INHERITS System.Object FINAL:
    METHOD PUBLIC STATIC INTEGER ToInt32(INPUT value AS CHARACTER).
    METHOD PUBLIC STATIC INT64 ToInt64(INPUT value AS CHARACTER).
    METHOD PUBLIC STATIC DECIMAL ToDecimal(INPUT value AS CHARACTER).
    METHOD PUBLIC STATIC LOGICAL ToBoolean(INPUT value AS CHARACTER).
    METHOD PUBLIC STATIC DATETIME ToDateTime(INPUT value AS CHARACTER).
    METHOD PUBLIC STATIC CHARACTER ToString(INPUT value AS System.Object).
    /* A number in another base: Convert:ToString(255, 16) is "ff". */
    METHOD PUBLIC STATIC CHARACTER ToString(INPUT value AS INTEGER, INPUT toBase AS INTEGER).
END CLASS.
