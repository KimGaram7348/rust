// Copyright 2012-2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Type encoding

#![allow(unused_must_use)] // as with encoding, everything is a no-fail MemWriter
#![allow(non_camel_case_types)]

use std::cell::RefCell;
use std::io::Cursor;
use std::io::prelude::*;

use rustc::hir::def_id::DefId;
use middle::region;
use rustc::ty::subst::Substs;
use rustc::ty::{self, Ty, TyCtxt};
use rustc::util::nodemap::FnvHashMap;

use rustc::hir;

use syntax::abi::Abi;
use syntax::ast;
use errors::Handler;

use rbml::leb128;
use encoder;

pub struct ctxt<'a, 'tcx: 'a> {
    pub diag: &'a Handler,
    // Def -> str Callback:
    pub ds: for<'b> fn(TyCtxt<'b, 'tcx, 'tcx>, DefId) -> String,
    // The type context.
    pub tcx: TyCtxt<'a, 'tcx, 'tcx>,
    pub abbrevs: &'a abbrev_map<'tcx>
}

impl<'a, 'tcx> encoder::EncodeContext<'a, 'tcx> {
    pub fn ty_str_ctxt<'b>(&'b self) -> ctxt<'b, 'tcx> {
        ctxt {
            diag: self.tcx.sess.diagnostic(),
            ds: encoder::def_to_string,
            tcx: self.tcx,
            abbrevs: &self.type_abbrevs
        }
    }
}

// Compact string representation for Ty values. API TyStr & parse_from_str.
// Extra parameters are for converting to/from def_ids in the string rep.
// Whatever format you choose should not contain pipe characters.
pub struct ty_abbrev {
    s: Vec<u8>
}

pub type abbrev_map<'tcx> = RefCell<FnvHashMap<Ty<'tcx>, ty_abbrev>>;

pub fn enc_ty<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>, t: Ty<'tcx>) {
    if let Some(a) = cx.abbrevs.borrow_mut().get(&t) {
        w.write_all(&a.s);
        return;
    }

    let pos = w.position();

    match t.sty {
        ty::TyBool => { write!(w, "b"); }
        ty::TyChar => { write!(w, "c"); }
        ty::TyNever => { write!(w, "!"); }
        ty::TyInt(t) => {
            match t {
                ast::IntTy::Is => write!(w, "is"),
                ast::IntTy::I8 => write!(w, "MB"),
                ast::IntTy::I16 => write!(w, "MW"),
                ast::IntTy::I32 => write!(w, "ML"),
                ast::IntTy::I64 => write!(w, "MD")
            };
        }
        ty::TyUint(t) => {
            match t {
                ast::UintTy::Us => write!(w, "us"),
                ast::UintTy::U8 => write!(w, "Mb"),
                ast::UintTy::U16 => write!(w, "Mw"),
                ast::UintTy::U32 => write!(w, "Ml"),
                ast::UintTy::U64 => write!(w, "Md")
            };
        }
        ty::TyFloat(t) => {
            match t {
                ast::FloatTy::F32 => write!(w, "Mf"),
                ast::FloatTy::F64 => write!(w, "MF"),
            };
        }
        ty::TyTrait(ref obj) => {
            write!(w, "x[");
            enc_existential_trait_ref(w, cx, obj.principal.0);
            enc_builtin_bounds(w, cx, &obj.builtin_bounds);

            enc_region(w, cx, obj.region_bound);

            for tp in &obj.projection_bounds {
                write!(w, "P");
                enc_existential_projection(w, cx, &tp.0);
            }

            write!(w, ".");
            write!(w, "]");
        }
        ty::TyTuple(ts) => {
            write!(w, "T[");
            for t in ts { enc_ty(w, cx, *t); }
            write!(w, "]");
        }
        ty::TyBox(typ) => { write!(w, "~"); enc_ty(w, cx, typ); }
        ty::TyRawPtr(mt) => { write!(w, "*"); enc_mt(w, cx, mt); }
        ty::TyRef(r, mt) => {
            write!(w, "&");
            enc_region(w, cx, r);
            enc_mt(w, cx, mt);
        }
        ty::TyArray(t, sz) => {
            write!(w, "V");
            enc_ty(w, cx, t);
            write!(w, "/{}|", sz);
        }
        ty::TySlice(t) => {
            write!(w, "V");
            enc_ty(w, cx, t);
            write!(w, "/|");
        }
        ty::TyStr => {
            write!(w, "v");
        }
        ty::TyFnDef(def_id, substs, f) => {
            write!(w, "F");
            write!(w, "{}|", (cx.ds)(cx.tcx, def_id));
            enc_substs(w, cx, substs);
            enc_bare_fn_ty(w, cx, f);
        }
        ty::TyFnPtr(f) => {
            write!(w, "G");
            enc_bare_fn_ty(w, cx, f);
        }
        ty::TyInfer(_) => {
            bug!("cannot encode inference variable types");
        }
        ty::TyParam(p) => {
            write!(w, "p[{}|{}]", p.idx, p.name);
        }
        ty::TyAdt(def, substs) => {
            write!(w, "a[{}|", (cx.ds)(cx.tcx, def.did));
            enc_substs(w, cx, substs);
            write!(w, "]");
        }
        ty::TyClosure(def, substs) => {
            write!(w, "k[{}|", (cx.ds)(cx.tcx, def));
            enc_substs(w, cx, substs.func_substs);
            for ty in substs.upvar_tys {
                enc_ty(w, cx, ty);
            }
            write!(w, ".");
            write!(w, "]");
        }
        ty::TyProjection(ref data) => {
            write!(w, "P[");
            enc_trait_ref(w, cx, data.trait_ref);
            write!(w, "{}]", data.item_name);
        }
        ty::TyAnon(def_id, substs) => {
            write!(w, "A[{}|", (cx.ds)(cx.tcx, def_id));
            enc_substs(w, cx, substs);
            write!(w, "]");
        }
        ty::TyError => {
            write!(w, "e");
        }
    }

    let end = w.position();
    let len = end - pos;

    let mut abbrev = Cursor::new(Vec::with_capacity(16));
    abbrev.write_all(b"#");
    {
        let start_position = abbrev.position() as usize;
        let bytes_written = leb128::write_unsigned_leb128(abbrev.get_mut(),
                                                          start_position,
                                                          pos);
        abbrev.set_position((start_position + bytes_written) as u64);
    }

    cx.abbrevs.borrow_mut().insert(t, ty_abbrev {
        s: if abbrev.position() < len {
            abbrev.get_ref()[..abbrev.position() as usize].to_owned()
        } else {
            // if the abbreviation is longer than the real type,
            // don't use #-notation. However, insert it here so
            // other won't have to `mark_stable_position`
            w.get_ref()[pos as usize .. end as usize].to_owned()
        }
    });
}

fn enc_mutability(w: &mut Cursor<Vec<u8>>, mt: hir::Mutability) {
    match mt {
        hir::MutImmutable => (),
        hir::MutMutable => {
            write!(w, "m");
        }
    };
}

fn enc_mt<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                    mt: ty::TypeAndMut<'tcx>) {
    enc_mutability(w, mt.mutbl);
    enc_ty(w, cx, mt.ty);
}

fn enc_opt<T, F>(w: &mut Cursor<Vec<u8>>, t: Option<T>, enc_f: F) where
    F: FnOnce(&mut Cursor<Vec<u8>>, T),
{
    match t {
        None => {
            write!(w, "n");
        }
        Some(v) => {
            write!(w, "s");
            enc_f(w, v);
        }
    }
}

pub fn enc_substs<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                            substs: &Substs<'tcx>) {
    write!(w, "[");
    for &k in substs.params() {
        if let Some(ty) = k.as_type() {
            write!(w, "t");
            enc_ty(w, cx, ty);
        } else if let Some(r) = k.as_region() {
            write!(w, "r");
            enc_region(w, cx, r);
        } else {
            bug!()
        }
    }
    write!(w, "]");
}

pub fn enc_generics<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                              generics: &ty::Generics<'tcx>) {
    enc_opt(w, generics.parent, |w, def_id| {
        write!(w, "{}|", (cx.ds)(cx.tcx, def_id));
    });
    write!(w, "{}|{}[",
           generics.parent_regions,
           generics.parent_types);

    for r in &generics.regions {
        enc_region_param_def(w, cx, r)
    }
    write!(w, "|");
    for t in &generics.types {
        enc_type_param_def(w, cx, t);
    }
    write!(w, "]");

    if generics.has_self {
        write!(w, "S");
    } else {
        write!(w, "N");
    }
}

pub fn enc_region(w: &mut Cursor<Vec<u8>>, cx: &ctxt, r: &ty::Region) {
    match *r {
        ty::ReLateBound(id, br) => {
            write!(w, "b[{}|", id.depth);
            enc_bound_region(w, cx, br);
            write!(w, "]");
        }
        ty::ReEarlyBound(ref data) => {
            write!(w, "B[{}|{}]",
                   data.index,
                   data.name);
        }
        ty::ReFree(ref fr) => {
            write!(w, "f[");
            enc_scope(w, cx, fr.scope);
            write!(w, "|");
            enc_bound_region(w, cx, fr.bound_region);
            write!(w, "]");
        }
        ty::ReScope(scope) => {
            write!(w, "s");
            enc_scope(w, cx, scope);
            write!(w, "|");
        }
        ty::ReStatic => {
            write!(w, "t");
        }
        ty::ReEmpty => {
            write!(w, "e");
        }
        ty::ReErased => {
            write!(w, "E");
        }
        ty::ReVar(_) | ty::ReSkolemized(..) => {
            // these should not crop up after typeck
            bug!("cannot encode region variables");
        }
    }
}

fn enc_scope(w: &mut Cursor<Vec<u8>>, cx: &ctxt, scope: region::CodeExtent) {
    match cx.tcx.region_maps.code_extent_data(scope) {
        region::CodeExtentData::CallSiteScope {
            fn_id, body_id } => write!(w, "C[{}|{}]", fn_id, body_id),
        region::CodeExtentData::ParameterScope {
            fn_id, body_id } => write!(w, "P[{}|{}]", fn_id, body_id),
        region::CodeExtentData::Misc(node_id) => write!(w, "M{}", node_id),
        region::CodeExtentData::Remainder(region::BlockRemainder {
            block: b, first_statement_index: i }) => write!(w, "B[{}|{}]", b, i),
        region::CodeExtentData::DestructionScope(node_id) => write!(w, "D{}", node_id),
    };
}

fn enc_bound_region(w: &mut Cursor<Vec<u8>>, cx: &ctxt, br: ty::BoundRegion) {
    match br {
        ty::BrAnon(idx) => {
            write!(w, "a{}|", idx);
        }
        ty::BrNamed(d, name, issue32330) => {
            write!(w, "[{}|{}|",
                   (cx.ds)(cx.tcx, d),
                   name);

            match issue32330 {
                ty::Issue32330::WontChange =>
                    write!(w, "n]"),
                ty::Issue32330::WillChange { fn_def_id, region_name } =>
                    write!(w, "y{}|{}]", (cx.ds)(cx.tcx, fn_def_id), region_name),
            };
        }
        ty::BrFresh(id) => {
            write!(w, "f{}|", id);
        }
        ty::BrEnv => {
            write!(w, "e|");
        }
    }
}

pub fn enc_trait_ref<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                               s: ty::TraitRef<'tcx>) {
    write!(w, "{}|", (cx.ds)(cx.tcx, s.def_id));
    enc_substs(w, cx, s.substs);
}

fn enc_existential_trait_ref<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                                       s: ty::ExistentialTraitRef<'tcx>) {
    write!(w, "{}|", (cx.ds)(cx.tcx, s.def_id));
    enc_substs(w, cx, s.substs);
}

fn enc_unsafety(w: &mut Cursor<Vec<u8>>, p: hir::Unsafety) {
    match p {
        hir::Unsafety::Normal => write!(w, "n"),
        hir::Unsafety::Unsafe => write!(w, "u"),
    };
}

fn enc_abi(w: &mut Cursor<Vec<u8>>, abi: Abi) {
    write!(w, "[");
    write!(w, "{}", abi.name());
    write!(w, "]");
}

pub fn enc_bare_fn_ty<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                                ft: &ty::BareFnTy<'tcx>) {
    enc_unsafety(w, ft.unsafety);
    enc_abi(w, ft.abi);
    enc_fn_sig(w, cx, &ft.sig);
}

pub fn enc_closure_ty<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                                ft: &ty::ClosureTy<'tcx>) {
    enc_unsafety(w, ft.unsafety);
    enc_fn_sig(w, cx, &ft.sig);
    enc_abi(w, ft.abi);
}

fn enc_fn_sig<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                        fsig: &ty::PolyFnSig<'tcx>) {
    write!(w, "[");
    for ty in &fsig.0.inputs {
        enc_ty(w, cx, *ty);
    }
    write!(w, "]");
    if fsig.0.variadic {
        write!(w, "V");
    } else {
        write!(w, "N");
    }
    enc_ty(w, cx, fsig.0.output);
}

fn enc_builtin_bounds(w: &mut Cursor<Vec<u8>>, _cx: &ctxt, bs: &ty::BuiltinBounds) {
    for bound in bs {
        match bound {
            ty::BoundSend => write!(w, "S"),
            ty::BoundSized => write!(w, "Z"),
            ty::BoundCopy => write!(w, "P"),
            ty::BoundSync => write!(w, "T"),
        };
    }

    write!(w, ".");
}

fn enc_type_param_def<'a, 'tcx>(w: &mut Cursor<Vec<u8>>, cx: &ctxt<'a, 'tcx>,
                                v: &ty::TypeParameterDef<'tcx>) {
    write!(w, "{}:{}|{}|{}|",
           v.name, (cx.ds)(cx.tcx, v.def_id),
           v.index, (cx.ds)(cx.tcx, v.default_def_id));
    enc_opt(w, v.default, |w, t| enc_ty(w, cx, t));
    enc_object_lifetime_default(w, cx, v.object_lifetime_default);
}

fn enc_region_param_def(w: &mut Cursor<Vec<u8>>, cx: &ctxt,
                        v: &ty::RegionParameterDef) {
    write!(w, "{}:{}|{}|",
           v.name, (cx.ds)(cx.tcx, v.def_id), v.index);
    for &r in &v.bounds {
        write!(w, "R");
        enc_region(w, cx, r);
    }
    write!(w, ".");
}

fn enc_object_lifetime_default<'a, 'tcx>(w: &mut Cursor<Vec<u8>>,
                                         cx: &ctxt<'a, 'tcx>,
                                         default: ty::ObjectLifetimeDefault)
{
    match default {
        ty::ObjectLifetimeDefault::Ambiguous => {
            write!(w, "a");
        }
        ty::ObjectLifetimeDefault::BaseDefault => {
            write!(w, "b");
        }
        ty::ObjectLifetimeDefault::Specific(r) => {
            write!(w, "s");
            enc_region(w, cx, r);
        }
    }
}

pub fn enc_predicate<'a, 'tcx>(w: &mut Cursor<Vec<u8>>,
                               cx: &ctxt<'a, 'tcx>,
                               p: &ty::Predicate<'tcx>)
{
    match *p {
        ty::Predicate::Trait(ref trait_ref) => {
            write!(w, "t");
            enc_trait_ref(w, cx, trait_ref.0.trait_ref);
        }
        ty::Predicate::Equate(ty::Binder(ty::EquatePredicate(a, b))) => {
            write!(w, "e");
            enc_ty(w, cx, a);
            enc_ty(w, cx, b);
        }
        ty::Predicate::RegionOutlives(ty::Binder(ty::OutlivesPredicate(a, b))) => {
            write!(w, "r");
            enc_region(w, cx, a);
            enc_region(w, cx, b);
        }
        ty::Predicate::TypeOutlives(ty::Binder(ty::OutlivesPredicate(a, b))) => {
            write!(w, "o");
            enc_ty(w, cx, a);
            enc_region(w, cx, b);
        }
        ty::Predicate::Projection(ty::Binder(ref data)) => {
            write!(w, "p");
            enc_trait_ref(w, cx, data.projection_ty.trait_ref);
            write!(w, "{}|", data.projection_ty.item_name);
            enc_ty(w, cx, data.ty);
        }
        ty::Predicate::WellFormed(data) => {
            write!(w, "w");
            enc_ty(w, cx, data);
        }
        ty::Predicate::ObjectSafe(trait_def_id) => {
            write!(w, "O{}|", (cx.ds)(cx.tcx, trait_def_id));
        }
        ty::Predicate::ClosureKind(closure_def_id, kind) => {
            let kind_char = match kind {
                ty::ClosureKind::Fn => 'f',
                ty::ClosureKind::FnMut => 'm',
                ty::ClosureKind::FnOnce => 'o',
            };
            write!(w, "c{}|{}|", (cx.ds)(cx.tcx, closure_def_id), kind_char);
        }
    }
}

fn enc_existential_projection<'a, 'tcx>(w: &mut Cursor<Vec<u8>>,
                                        cx: &ctxt<'a, 'tcx>,
                                        data: &ty::ExistentialProjection<'tcx>) {
    enc_existential_trait_ref(w, cx, data.trait_ref);
    write!(w, "{}|", data.item_name);
    enc_ty(w, cx, data.ty);
}
