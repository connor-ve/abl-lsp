USING oo.*.

/* Contract errors: no Corners(), Area() without OVERRIDE, OVERRIDE of nothing. */
CLASS oo.Broken INHERITS Shape IMPLEMENTS IHasCorners:
    METHOD PUBLIC DECIMAL Area():
        RETURN 0.
    END METHOD.

    METHOD PUBLIC OVERRIDE CHARACTER Explode():
        RETURN "".
    END METHOD.
END CLASS.
