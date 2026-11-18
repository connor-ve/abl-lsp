/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
CLASS System.IO.Directory INHERITS System.Object FINAL:
    METHOD PUBLIC STATIC LOGICAL Exists(INPUT path AS CHARACTER).
    /* Returns a System.IO.DirectoryInfo. */
    METHOD PUBLIC STATIC System.Object CreateDirectory(INPUT path AS CHARACTER).
    METHOD PUBLIC STATIC VOID Delete(INPUT path AS CHARACTER).
    METHOD PUBLIC STATIC VOID Delete(INPUT path AS CHARACTER, INPUT recursive AS LOGICAL).
    METHOD PUBLIC STATIC VOID Move(INPUT sourceDirName AS CHARACTER, INPUT destDirName AS CHARACTER).
    METHOD PUBLIC STATIC CHARACTER GetCurrentDirectory().
END CLASS.
