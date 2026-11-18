/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
CLASS System.Guid INHERITS System.Object FINAL:
    DEFINE PUBLIC STATIC PROPERTY Empty AS System.Guid NO-UNDO GET.

    METHOD PUBLIC STATIC System.Guid NewGuid().
    METHOD PUBLIC STATIC System.Guid Parse(INPUT s AS CHARACTER).
    METHOD PUBLIC OVERRIDE CHARACTER ToString().
    /* "N" for 32 digits, "D" with hyphens (the default), "B" in braces. */
    METHOD PUBLIC CHARACTER ToString(INPUT fmt AS CHARACTER).
END CLASS.
