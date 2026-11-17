USING app.*.
USING Progress.Lang.*.

CLASS app.OrderService INHERITS BaseService IMPLEMENTS IOrderProcessor:

    DEFINE PRIVATE TEMP-TABLE ttOrder NO-UNDO
        FIELD OrderNum AS INTEGER
        FIELD Status   AS CHARACTER
        INDEX idxOrder IS PRIMARY OrderNum.

    DEFINE PUBLIC PROPERTY LastStatus AS OrderStatus NO-UNDO
        GET():
            RETURN OrderStatus:Pending.
        END GET.

    CONSTRUCTOR PUBLIC OrderService ():
        SUPER("orders").
    END CONSTRUCTOR.

    METHOD PUBLIC INTEGER ProcessOrders (INPUT piMax AS INTEGER):
        DEFINE VARIABLE iDone AS INTEGER NO-UNDO.

        DO iDone = 1 TO piMax:
            CREATE ttOrder.
            ttOrder.OrderNum = iDone.
        END.

        THIS-OBJECT:Log(SUBSTITUTE("Processed &1 orders", iDone)).
        RETURN iDone.

        CATCH e AS Progress.Lang.Error:
            Log(e:GetMessage(1)).
            RETURN 0.
        END CATCH.
    END METHOD.

END CLASS.
