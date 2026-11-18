/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
/* Generic: DEFINE VARIABLE oList AS "System.Collections.Generic.List<CHARACTER>" NO-UNDO.
   T stands for the element type in the angle brackets. */
CLASS System.Collections.Generic.List INHERITS System.Object:
    DEFINE PUBLIC PROPERTY Count AS INTEGER NO-UNDO GET.
    DEFINE PUBLIC PROPERTY Capacity AS INTEGER NO-UNDO GET. SET.

    CONSTRUCTOR PUBLIC List():
    END CONSTRUCTOR.
    CONSTRUCTOR PUBLIC List(INPUT capacity AS INTEGER):
    END CONSTRUCTOR.

    METHOD PUBLIC VOID Add(INPUT item AS T).
    METHOD PUBLIC VOID Insert(INPUT index AS INTEGER, INPUT item AS T).
    METHOD PUBLIC LOGICAL Remove(INPUT item AS T).
    METHOD PUBLIC VOID RemoveAt(INPUT index AS INTEGER).
    METHOD PUBLIC VOID Clear().
    METHOD PUBLIC LOGICAL Contains(INPUT item AS T).
    METHOD PUBLIC INTEGER IndexOf(INPUT item AS T).
    METHOD PUBLIC VOID Sort().
    METHOD PUBLIC VOID Reverse().
END CLASS.
