use super::*;

#[test]
fn layer_and_font_queries_allow_incremental_gc_to_finish() {
    use tjs_core::{Value, member};

    let (mut engine, mut host, _) = setup();
    run(
        &mut engine,
        &mut host,
        "var w=new Window(),r=new Layer(w,null),f=r.font; f.height=24; \
         var keep=[];for(var i=0;i<200;i++) keep.add(%['value'=>i]);'ok';",
        10000,
    );
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    let r = heap.intern(&"r".encode_utf16().collect::<Vec<_>>());
    let f = heap.intern(&"f".encode_utf16().collect::<Vec<_>>());
    let layer = heap.member(global, r).unwrap().unwrap();
    let font = heap.member(global, f).unwrap().unwrap();
    let mut roots = vec![Value::Obj(global.into()), layer, font];
    let mut queries = Vec::new();
    for (owner, names) in [
        (
            layer,
            "font type left top width height imageLeft imageTop imageWidth imageHeight clipLeft clipTop clipWidth clipHeight opacity face visible holdAlpha",
        ),
        (
            font,
            "face height angle bold italic strikeout underline faceIsFileName",
        ),
    ] {
        for name in names.split_whitespace() {
            let key = Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>()));
            roots.push(key);
            queries.push((owner, key));
        }
    }
    let mut completed = false;
    for _ in 0..100_000 {
        for &(owner, key) in &queries {
            member::get(heap, owner, key).unwrap();
        }
        if heap
            .collect_step(roots.iter().copied(), 1)
            .completed
            .is_some()
        {
            completed = true;
            break;
        }
    }
    assert!(
        completed,
        "read-only layer/font getters kept retracing their owners"
    );
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "keep[199].value+' '+r.width+' '+f.height;",
            10000
        ),
        "199 32 24"
    );
}

#[test]
fn layer_order_exchange_and_swap_preserve_reference_tree_positions() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var w=new Window(),r=new Layer(w,null);
            var a=new Layer(w,r),b=new Layer(w,r),c=new Layer(w,r);
            var x=new Layer(w,a),y=new Layer(w,b),z=new Layer(w,c);
            a.left=7;b.left=11;a.visible=true;b.visible=false;
            var cached=r.children;
            a.exchange(c);
            if(r.children!==cached || cached[0]!==c || cached[2]!==a || x.parent!==a || z.parent!==c) throw 'exchange siblings';
            if(a.left!=7 || b.left!=11 || !a.visible || b.visible) throw 'exchange metadata';
            a.swap(b);
            if(r.children[1]!==a || r.children[2]!==b || x.parent!==b || y.parent!==a) throw 'swap children';
            a.absoluteOrder=100;
            if(!r.absoluteOrderMode || a.absoluteOrder!=100 || a.order!=2 || b.absoluteOrder!=2) throw 'absolute mode';
            c.absoluteOrder=100;
            if(c.order!=1 || a.order!=2) throw 'absolute equal before';
            a.absoluteOrder=-10;
            if(a.order!=0 || a.absoluteOrder!=-10) throw 'negative order';
            a.moveBefore(c);
            if(r.absoluteOrderMode || a.order!=2) throw 'move before';
            a.moveBehind(b);
            if(a.order!=0) throw 'move behind';
            a.order=999;
            if(a.order!=2) throw 'relative clamp';
            var errors=0;
            try{r.order=1;}catch(e){errors++;}
            try{a.moveBefore(a);}catch(e){errors++;}
            try{x.moveBehind(y);}catch(e){errors++;}
            if(errors!=3) throw 'order errors';
            // Different parents retain each layer's own old rank.
            var p=new Layer(w,r),q=new Layer(w,r);
            var p0=new Layer(w,p),p1=new Layer(w,p),q0=new Layer(w,q),q1=new Layer(w,q),q2=new Layer(w,q);
            p0.exchange(q2);
            if(p0.parent!==q || q2.parent!==p || p0.order!=0 || q2.order!=1) throw 'cross-parent ranks';
            // Child absolute ranks survive Swap only when both modes agree.
            p.absoluteOrderMode=q.absoluteOrderMode=true;
            p1.absoluteOrder=-8;p0.absoluteOrder=50;
            p.swap(q);
            if(p1.parent!==q || p1.absoluteOrder!=-8 || p0.parent!==p || p0.absoluteOrder!=50) throw 'child absolute ranks';
            'ok';
        "#,
                slice
            ),
            "ok"
        );
        engine.reset();
    }
}

#[test]
fn ancestor_and_primary_exchanges_break_bridges_before_joining() {
    for keep in [false, true] {
        for primary in [false, true] {
            let (mut engine, mut host, _) = setup();
            let script = format!(
                r#"
                var w=new Window(),r=new Layer(w,null),a=new Layer(w,r);
                var b=new Layer(w,a),c=new Layer(w,b),x=new Layer(w,a),y=new Layer(w,c);
                var outer={outer};
                outer.{method}(c);
                if({primary}) {{
                    if(w.primaryLayer!==c || !c.isPrimary || r.isPrimary || !c.visible || c.opacity!=255) throw 'primary';
                    if(r.parent!==b || a.parent!==c) throw 'primary bridge';
                    r.left=3;r.opacity=12;r.visible=false;
                }} else {{
                    if(c.parent!==r || a.parent!==b || b.parent!==c) throw 'ancestor bridge';
                    if(x.parent!=={x_parent} || y.parent!=={y_parent}) throw 'ancestor children';
                }}
                // Parent-child exchanges have no intermediate bridge node.
                var d=new Layer(w,c),e=new Layer(w,d),f=new Layer(w,e);
                d.{method}(e);
                if(d.parent!==e || e.parent!==c || f.parent!=={f_parent}) throw 'direct ancestry';
                var w2=new Window(),r2=new Layer(w2,null),errors=0;
                try{{c.exchange(r2);}}catch(err){{errors++;}}
                if(errors!=1) throw 'cross-window';
                'ok';
            "#,
                outer = if primary { "r" } else { "a" },
                method = if keep { "swap" } else { "exchange" },
                primary = i32::from(primary),
                x_parent = if keep { "c" } else { "a" },
                y_parent = if keep { "a" } else { "c" },
                f_parent = if keep { "d" } else { "e" }
            );
            assert_eq!(run(&mut engine, &mut host, &script, 1), "ok");
            engine.reset();
        }
    }
}

#[test]
fn exchange_blur_callbacks_can_wait_throw_and_be_retried() {
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var w=new Window(),r=new Layer(w,null),a=new Layer(w,r),b=new Layer(w,r),c=new Layer(w,r);
        a.visible=b.visible=c.visible=true;a.focusable=b.focusable=c.focusable=true;
        a.focus();var token=%[message:'exchange'],caught=false,seen=0;
        a.onBlur=function(next){
            if(this.parent!==global.r) throw 'severed too early';
            global.seen++;System.wait(0);throw global.token;
        };
        try{a.swap(b);}catch(e){caught=e===token;}
        if(!caught || seen!=1 || a.parent!==r || b.parent!==r || !b.focused) throw 'callback exception';
        a.onBlur=Layer.onBlur incontextof a;
        a.swap(b);
        if(r.children[0]!==b || r.children[1]!==a) throw 'retry';
        b.onBlur=function(next){System.wait(0);global.seen++;};
        b.focus();r.exchange(c);
        if(w.primaryLayer!==c || seen!=2 || a.focused || b.focused) throw 'primary focus detach';
        'ok';
    "#,
            1
        ),
        "ok"
    );
    engine.reset();
}
