/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
/* .NET exceptions reach ABL as errors: CATCH e AS System.Exception. */
CLASS System.Exception INHERITS System.Object:
    DEFINE PUBLIC PROPERTY Message AS CHARACTER NO-UNDO GET.
    DEFINE PUBLIC PROPERTY StackTrace AS CHARACTER NO-UNDO GET.
    DEFINE PUBLIC PROPERTY Source AS CHARACTER NO-UNDO GET. SET.
    DEFINE PUBLIC PROPERTY InnerException AS System.Exception NO-UNDO GET.

    CONSTRUCTOR PUBLIC Exception():
    END CONSTRUCTOR.
    CONSTRUCTOR PUBLIC Exception(INPUT message AS CHARACTER):
    END CONSTRUCTOR.
END CLASS.
