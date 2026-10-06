use super::*;

#[test]
fn legacy_menu_scripts_keep_state_callbacks_and_lifetime_without_native_ui() {
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var w=new Window(), root=w.menu, clicks=0, gone=0, last=null;
        class Item extends MenuItem {
            function Item(owner, caption){super.MenuItem(owner,caption);}
            function finalize(){global.gone++;System.wait(0);}
        }
        var owner=%[action:function(e){global.clicks++;global.last=e;global.System.wait(0);}];
        var a=new Item(owner,'A'), b=new Item(owner,'B');
        if(root!==w.menu || root.window!==w || root.parent!==null || root.root!==root) throw 'root';
        root.add(a);var list=root.children;root.insert(b,0);
        if(list.count!=1 || root.children!==list || list.count!=2 || list[0]!==b) throw 'children cache';
        a.index=0;root.add(a);
        if(root.children[0]!==a || b.index!=1 || root.children.count!=2) throw 'order';
        if(a.root!==root || a.parent!==root || a.window!==null) throw 'parent';
        a.group=b.group=7;a.radio=b.radio=true;a.checked=true;b.checked=true;
        if(a.checked || !b.checked) throw 'radio';
        a.caption='updated';a.shortcut='Ctrl+A';a.visible=false;
        if(a.caption!='updated' || a.shortcut!='Ctrl+A' || a.visible) throw 'state';
        if(root.HMENU!=0 || a.popup(0,10,20)!=0 || clicks!=0) throw 'native UI';
        a.fireClick();if(clicks!=0) throw 'hidden window';
        w.visible=true;a.fireClick();
        if(clicks!=1 || last.type!='onClick' || last.target!==a) throw 'action';
        root.enabled=false;a.fireClick();a.onClick();
        if(clicks!=2) throw 'explicit action';root.enabled=true;
        var caught=0;
        try{a.add(root);}catch(e){caught++;}
        try{root.insert(new MenuItem(null),-1);}catch(e){caught++;}
        try{a.HMENU=1;}catch(e){caught++;}
        if(caught!=3) throw 'invalid operation';
        root.remove(a);if(a.parent!==null || a.root!==a) throw 'detach';
        root.add(a);invalidate b;
        if(root.children.count!=1 || root.children[0]!==a || gone!=1) throw 'child cleanup';
        var extra=new MenuItem(null,w);
        invalidate w;
        if(isvalid root || isvalid a || isvalid extra || gone!=2) throw 'window cleanup';
        42;
    "#,
            1
        ),
        "42"
    );
    host.service();
    assert!(host.windows.is_empty());
    assert_eq!(engine.pending_operations(), 0);

    // Both failure and cancellation leave remaining children available for retry.
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var parent=new MenuItem(null), fail=true;
        class RetryItem extends MenuItem {
            function RetryItem(){super.MenuItem(null);}
            function finalize(){if(global.fail) throw 'retry';System.wait(0);}
        }
        var child=new RetryItem();parent.add(child);
        try{invalidate parent;}catch(e){}
        if(!isvalid parent || !isvalid child || parent.children.count!=1) throw 'retry state';
        fail=false;invalidate parent;
        if(isvalid child) throw 'retry cleanup';
        var outer=new MenuItem(null);
        class NestedItem extends MenuItem {
            function NestedItem(){super.MenuItem(null);}
            function finalize(){invalidate global.outer;System.wait(0);}
        }
        var nested=new NestedItem();outer.add(nested);invalidate nested;
        if(isvalid outer || isvalid nested) throw 'nested cleanup';
        var cancelRoot=new MenuItem(null), hold=true;
        class CancelItem extends MenuItem {
            function CancelItem(){super.MenuItem(null);}
            function finalize(){if(global.hold) System.wait(100000);}
        }
        var cancelChild=new CancelItem();cancelRoot.add(cancelChild);42;
    "#,
            1
        ),
        "42"
    );
    let id = submit(&mut engine, "invalidate cancelRoot;");
    let mut waiting = false;
    for _ in 0..1000 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Waiting { .. } => {
                waiting = true;
                break;
            }
            EngineEvent::Yielded => {}
            event => panic!("menu cleanup: {event:?}"),
        }
    }
    assert!(waiting);
    engine.cancel(id);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "hold=false;invalidate cancelRoot;if(isvalid cancelChild) throw 'cancel cleanup';42;",
            1
        ),
        "42"
    );

    // Optional local integration consumes the original script unchanged. Normal
    // builds/tests have no dependency on the ignored reference checkouts.
    if let Some(path) = std::env::var_os("KRKR_KAG_MENU_REFERENCE") {
        let script = std::fs::read_to_string(path).expect("read requested KAG Menus.tjs");
        run(&mut engine, &mut host, &script, 128);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var kw=new Window(), commands=0, clicked=null;
            kw.visible=true;kw.autoRecordPageShowing=false;kw.freeSaveDataMode=false;
            kw.autoModePageWaits=%[fast:1,faster:2,medium:3,slower:4,slow:5];
            kw.autoModeLineWaits=%[fast:1,faster:2,medium:3,slower:4,slow:5];
            kw.chSpeeds=%[fast:1,normal:2,slow:3];
            var names=['onRightClickMenuItemClick','onShowHistoryMenuItemClick',
                'onSkipToNextStopMenuItemClick','onAutoModeMenuItemClick','onAutoModeWaitMenuClick',
                'onBackStartMenuItemClick','onGoToStartMenuItemClick','onExitMenuItemClick',
                'onChSpeedMenuItemClick','onChNonStopToPageBreakItemClick','onCh2ndSpeedMenuItemClick',
                'onCh2ndNonStopToPageBreakItemClick','onChAntialiasMenuItemClick','onChChangeFontMenuItem',
                'onRestoreMenuClick','onStoreMenuClick','onWindowedMenuItemClick','onFullScreenMenuItemClick',
                'onHelpIndexMenuItemClick','onHelpAboutMenuItemClick','onReloadScenarioMenuItemClick',
                'onShowConsoleMenuItemClick','onShowContollerMenuItemClick'];
            for(var i=0;i<names.count;i++) kw[names[i]]=function(item){global.commands++;global.clicked=item;System.wait(0);};
            (KAGWindow_createMenus incontextof kw)();
            if(kw.menu.children.count!=7 || kw.autoModeMenuItem.parent!==kw.systemMenu) throw 'KAG tree';
            kw.autoModeMenuItem.click();kw.autoModeMenuItem.fireClick();
            if(commands!=2 || clicked!==kw.autoModeMenuItem) throw 'KAG command';
            kw.chFastMenuItem.checked=true;kw.chSlowMenuItem.checked=true;
            if(kw.chFastMenuItem.checked || !kw.chSlowMenuItem.checked) throw 'KAG radio';
            kw.autoModeMenuItem.accessible=false;kw.autoModeMenuItem.fireClick();
            if(commands!=2 || kw.autoModeMenuItem.enabled!=true) throw 'KAG access';
            var retained=kw.autoModeMenuItem;invalidate kw;
            if(isvalid retained) throw 'KAG lifetime';42;
        "#,
                64
            ),
            "42"
        );
    }
    host.service();
    assert!(host.windows.is_empty());
    assert_eq!(engine.pending_operations(), 0);
}
