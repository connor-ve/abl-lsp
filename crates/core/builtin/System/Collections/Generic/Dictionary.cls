/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
/* Generic: "System.Collections.Generic.Dictionary<CHARACTER, INTEGER>".
   TKey and TValue stand for the types in the angle brackets. */
CLASS System.Collections.Generic.Dictionary INHERITS System.Object:
    DEFINE PUBLIC PROPERTY Count AS INTEGER NO-UNDO GET.

    CONSTRUCTOR PUBLIC Dictionary():
    END CONSTRUCTOR.

    METHOD PUBLIC VOID Add(INPUT key AS TKey, INPUT value AS TValue).
    METHOD PUBLIC LOGICAL Remove(INPUT key AS TKey).
    METHOD PUBLIC VOID Clear().
    METHOD PUBLIC LOGICAL ContainsKey(INPUT key AS TKey).
    METHOD PUBLIC LOGICAL ContainsValue(INPUT value AS TValue).
    METHOD PUBLIC LOGICAL TryGetValue(INPUT key AS TKey, OUTPUT value AS TValue).
END CLASS.
