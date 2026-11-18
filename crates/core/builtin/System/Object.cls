/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
/* The root of every .NET class; to ABL also a Progress.Lang.Object. */
CLASS System.Object INHERITS Progress.Lang.Object:
    CONSTRUCTOR PUBLIC Object():
    END CONSTRUCTOR.

    METHOD PUBLIC OVERRIDE CHARACTER ToString().
    METHOD PUBLIC LOGICAL Equals(INPUT obj AS System.Object).
    METHOD PUBLIC INTEGER GetHashCode().
END CLASS.
