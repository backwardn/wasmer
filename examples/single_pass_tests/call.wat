(module
    (func $main (export "main")
        (local $a i32)
        (block
            (set_local $a (i32.const 33))
            (i32.const 11)
            (call $foo (get_local $a))
            (i32.add)
            (i32.const 86)
            (i32.eq)
            (br_if 0)
            (unreachable)
        )
    )

    (func $foo (param $input i32) (result i32)
        (local $a i32)
        (set_local $a (i32.const 42))
        (get_local $a)
        (get_local $input)
        (i32.add)
    )
)
