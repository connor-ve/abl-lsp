USING Progress.Lang.*.

CLASS app.BaseService ABSTRACT:

    DEFINE PROTECTED PROPERTY ServiceName AS CHARACTER NO-UNDO
        GET.
        SET.

    CONSTRUCTOR PUBLIC BaseService (INPUT pcName AS CHARACTER):
        ServiceName = pcName.
    END CONSTRUCTOR.

    METHOD PROTECTED VOID Log (INPUT pcMessage AS CHARACTER):
        MESSAGE SUBSTITUTE("[&1] &2", ServiceName, pcMessage).
    END METHOD.

END CLASS.
