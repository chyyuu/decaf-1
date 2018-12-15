use super::ast::*;
use super::types::*;
use super::symbol::*;
use super::util::*;
use super::tac::*;
use super::print::quote;

use std::default::Default as D;

pub struct TacCodeGen {
  cur_method: *mut Vec<Tac>,
  break_stack: Vec<i32>,
  methods: Vec<TacMethod>,
  reg_cnt: i32,
  label_cnt: i32,
  cur_this: i32,
}

impl TacCodeGen {
  pub fn gen(&mut self, program: &mut Program) -> TacProgram {
    self.program(program);
    unimplemented!()
  }

  fn new_reg(&mut self) -> i32 {
    self.reg_cnt += 1;
    self.reg_cnt
  }

  fn new_label(&mut self) -> i32 {
    self.label_cnt += 1;
    self.label_cnt
  }

  fn array_length(&mut self, array: i32) -> i32 {
    let ret = self.new_reg();
    self.push(Tac::Load { dst: ret, base: array, offset: -INT_SIZE });
    ret
  }

  fn array_at(&mut self, array: i32, index: i32) -> i32 {
    let (ret, int_size, offset) = (self.new_reg(), self.new_reg(), self.new_reg());
    self.push(Tac::IntConst(int_size, INT_SIZE));
    self.push(Tac::Mul(offset, index, int_size));
    self.push(Tac::Add(offset, array, offset));
    self.push(Tac::Load { dst: ret, base: offset, offset: 0 });
    ret
  }

  fn intrinsic_call(&mut self, call: IntrinsicCall) -> i32 {
    let ret = if call.ret { self.new_reg() } else { -1 };
    self.push(Tac::DirectCall(ret, call.name.to_owned()));
    ret
  }

  fn push(&mut self, tac: Tac) {
    self.cur_method.get().push(tac);
  }
}

fn resolve_field_order(class_def: &mut ClassDef) {
  if class_def.field_cnt >= 0 { return; } // already handled
  let mut field_cnt = if class_def.p_ptr.is_null() { 0 } else {
    let p = class_def.p_ptr.get();
    resolve_field_order(p);
    class_def.v_tbl = p.v_tbl.clone();
    p.field_cnt
  };
  'out: for field in &mut class_def.field {
    match field {
      FieldDef::MethodDef(method_def) => if !method_def.static_ {
        let p = class_def.p_ptr.get();
        for p_method in &p.v_tbl.methods {
          if p_method.get().name == method_def.name {
            method_def.offset = p_method.get().offset;
            class_def.v_tbl.methods[method_def.offset as usize] = method_def;
            continue 'out;
          }
        }
        method_def.offset = class_def.v_tbl.methods.len() as i32;
        class_def.v_tbl.methods.push(method_def);
      }
      FieldDef::VarDef(var_def) => {
        var_def.offset = field_cnt;
        field_cnt += 1;
      }
    }
  }
}

impl TacCodeGen {
  fn program(&mut self, program: &mut Program) {
    for class_def in &mut program.class {
      resolve_field_order(class_def);
      for field_def in &mut class_def.field {
        if let FieldDef::MethodDef(method_def) = field_def {
          // "this" is already inserted as 1st by symbol builder
          for param in &mut method_def.param {
            param.offset = self.new_reg();
          }
        }
      }
      self.methods.push(TacMethod { name: format!("_{}_New", class_def.name), ..D::default() });
      self.cur_method = &mut self.methods.last_mut().unwrap().code;
      let size = self.new_reg();
      self.push(Tac::IntConst(size, (class_def.field_cnt + 1) * INT_SIZE));
      self.push(Tac::Param(size));
      let ret = self.intrinsic_call(ALLOCATE);
      let v_tbl = self.new_reg();
      self.push(Tac::LoadVTbl(v_tbl, class_def.name));
      self.push(Tac::Store { base: ret, offset: 0, src: v_tbl });
      let zero = self.new_reg();
      self.push(Tac::IntConst(zero, 0));
      for i in 0..class_def.field_cnt {
        self.push(Tac::Store { base: ret, offset: (i + 1) * INT_SIZE, src: zero });
      }
      self.push(Tac::Ret(ret));
      for field_def in &mut class_def.field {
        if let FieldDef::MethodDef(method_def) = field_def {
          if !method_def.static_ {
            self.cur_this = method_def.param[0].offset;
          }
          self.block(&mut method_def.body);
        }
      }
    }
  }

  fn stmt(&mut self, stmt: &mut Stmt) {
    use ast::Stmt::*;
    match stmt {
      Simple(simple) => self.simple(simple),
      If(if_) => {
        let before_else = self.new_label();
        self.expr(&mut if_.cond);
        self.push(Tac::Je(if_.cond.reg, before_else));
        self.block(&mut if_.on_true);
        if let Some(on_false) = &mut if_.on_false {
          let after_else = self.new_label();
          self.push(Tac::Jmp(after_else));
          self.push(Tac::Label(before_else));
          self.block(on_false);
          self.push(Tac::Label(after_else));
        } else {
          self.push(Tac::Label(before_else));
        }
      }
      While(while_) => {
        let (before_cond, after_body) = (self.new_label(), self.new_label());
        self.push(Tac::Label(before_cond));
        self.expr(&mut while_.cond);
        self.push(Tac::Je(while_.cond.reg, after_body));
        self.break_stack.push(after_body);
        self.block(&mut while_.body);
        self.break_stack.pop();
        self.push(Tac::Jmp(before_cond));
        self.push(Tac::Label(after_body));
      }
      For(for_) => {
        let (before_cond, after_body) = (self.new_label(), self.new_label());
        self.simple(&mut for_.init);
        self.push(Tac::Label(before_cond));
        self.expr(&mut for_.cond);
        self.push(Tac::Je(for_.cond.reg, after_body));
        self.break_stack.push(after_body);
        self.block(&mut for_.body);
        self.break_stack.pop();
        self.simple(&mut for_.update);
        self.push(Tac::Jmp(before_cond));
        self.push(Tac::Label(after_body));
      }
      Return(return_) => if let Some(expr) = &mut return_.expr {
        self.expr(expr);
        self.push(Tac::Ret(expr.reg));
      } else {
        self.push(Tac::Ret(-1));
      }
      Print(print) => for expr in &mut print.print {
        self.expr(expr);
        self.push(Tac::Param(expr.reg));
        match &expr.type_ {
          SemanticType::Int => { self.intrinsic_call(PRINT_INT); }
          SemanticType::Bool => { self.intrinsic_call(PRINT_BOOL); }
          SemanticType::String => { self.intrinsic_call(PRINT_STRING); }
          _ => unreachable!(),
        }
      }
      Break(_) => {
        let after_loop = *self.break_stack.last().unwrap();
        self.push(Tac::Jmp(after_loop));
      }
      SCopy(s_copy) => {
        self.expr(&mut s_copy.src);
        let new_obj = self.new_reg();
        let tmp = self.new_reg();
        let class = s_copy.src.type_.get_class();
        self.push(Tac::DirectCall(new_obj, format!("_{}_New", class.name)));
        for i in 0..class.field_cnt {
          self.push(Tac::Load { dst: tmp, base: s_copy.src.reg, offset: (i + 1) * INT_SIZE });
          self.push(Tac::Store { base: new_obj, offset: (i + 1) * INT_SIZE, src: tmp });
        }
        self.push(Tac::Assign(s_copy.dst_sym.get().offset, new_obj));
      }
      Foreach(foreach) => {
        self.expr(&mut foreach.arr);
        foreach.def.offset = self.new_reg();
        let (x, i, one, cmp) = (self.new_reg(), self.new_reg(), self.new_reg(), self.new_reg());
        let (before_cond, after_body) = (self.new_label(), self.new_label());
        self.push(Tac::IntConst(i, 0));
        self.push(Tac::IntConst(one, 1));
        self.push(Tac::Label(before_cond));
        let array_length = self.array_length(foreach.arr.reg);
        self.push(Tac::Le(cmp, i, array_length));
        self.push(Tac::Je(cmp, after_body));
        let array_elem = self.array_at(foreach.arr.reg, i);
        self.push(Tac::Assign(x, array_elem));
        if let Some(cond) = &mut foreach.cond {
          self.expr(cond);
          self.push(Tac::Je(cond.reg, after_body));
        }
        self.break_stack.push(after_body);
        self.block(&mut foreach.body);
        self.break_stack.pop();
        self.push(Tac::Add(i, i, one));
        self.push(Tac::Jmp(before_cond));
        self.push(Tac::Label(after_body));
      }
      Guarded(guarded) => for (e, b) in &mut guarded.guarded {
        self.expr(e);
        let after_body = self.new_label();
        self.push(Tac::Je(e.reg, after_body));
        self.block(b);
        self.push(Tac::Label(after_body));
      }
      Block(block) => self.block(block),
    }
  }

  fn simple(&mut self, simple: &mut Simple) {
    match simple {
      Simple::Assign(assign) => {
        self.expr(&mut assign.dst);
        self.expr(&mut assign.src);
        match &assign.dst.data {
          ExprData::Id(id) => {
            let var_def = id.symbol.get();
            match var_def.scope.get().kind {
              ScopeKind::Local(_) | ScopeKind::Parameter(_) => { self.push(Tac::Assign(var_def.offset, assign.src.reg)); }
              ScopeKind::Class(_) => {
                self.push(Tac::Store { base: id.owner.as_ref().unwrap().reg, offset: (var_def.offset + 1) * INT_SIZE, src: assign.src.reg });
              }
              _ => unreachable!(),
            }
          }
          ExprData::Indexed(indexed) => {
            let (int_size, offset) = (self.new_reg(), self.new_reg());
            self.push(Tac::IntConst(int_size, INT_SIZE));
            self.push(Tac::Mul(offset, indexed.idx.reg, int_size));
            self.push(Tac::Add(offset, indexed.arr.reg, offset));
            self.push(Tac::Store { base: offset, offset: 0, src: assign.src.reg });
          }
          _ => unreachable!(),
        }
      }
      Simple::VarDef(var_def) => {
        var_def.offset = self.new_reg();
        if let Some(src) = &mut var_def.src {
          self.expr(src);
          self.push(Tac::Assign(var_def.offset, src.reg));
        }
      }
      Simple::Expr(expr) => self.expr(expr),
      Simple::Skip => {}
    }
  }

  fn block(&mut self, block: &mut Block) {
    for stmt in &mut block.stmt { self.stmt(stmt); }
  }

  fn expr(&mut self, expr: &mut Expr) {
    use ast::ExprData::*;
    match &mut expr.data {
      Id(id) => {
        let var_def = id.symbol.get();
        match var_def.scope.get().kind {
          ScopeKind::Local(_) | ScopeKind::Parameter(_) => expr.reg = var_def.offset,
          ScopeKind::Class(_) => {
            let owner = id.owner.as_mut().unwrap();
            self.expr(owner);
            expr.reg = self.new_reg();
            self.push(Tac::Load { dst: expr.reg, base: owner.reg, offset: (var_def.offset + 1) * INT_SIZE });
          }
          _ => unreachable!(),
        };
      }
      Indexed(Indexed) => {}
      IntConst(v) => {
        expr.reg = self.new_reg();
        self.push(Tac::IntConst(expr.reg, *v));
      }
      BoolConst(v) => {
        expr.reg = self.new_reg();
        self.push(Tac::IntConst(expr.reg, if *v { 1 } else { 0 }));
      }
      StringConst(v) => {
        expr.reg = self.new_reg();
        self.push(Tac::StrConst(expr.reg, quote(v)));
      }
      ArrayConst(_) => unimplemented!(),
      Null => {
        expr.reg = self.new_reg();
        self.push(Tac::IntConst(expr.reg, 0));
      }
      Call(call) => if call.is_arr_len {
        let owner = call.owner.as_mut().unwrap();
        self.expr(owner);
        expr.reg = self.array_length(owner.reg);
      } else {
        let method = call.method.get();
        let class = method.class.get();
        expr.reg = if method.ret_t.sem != VOID { self.new_reg() } else { -1 };
        if method.static_ {
          for arg in &mut call.arg {
            self.expr(arg);
            self.push(Tac::Param(arg.reg));
          }
          self.push(Tac::DirectCall(expr.reg, format!("_{}.{}", class.name, method.name)));
        } else {
          let owner = call.owner.as_mut().unwrap();
          self.expr(owner);
          self.push(Tac::Param(owner.reg));
          for arg in &mut call.arg {
            self.expr(arg);
            self.push(Tac::Param(arg.reg));
          }
          let slot = self.new_reg();
          self.push(Tac::Load { dst: slot, base: owner.reg, offset: 0 });
          self.push(Tac::Load { dst: slot, base: slot, offset: method.offset * INT_SIZE });
          self.push(Tac::IndirectCall(expr.reg, slot));
        }
      }
      Unary(unary) => {
        self.expr(&mut unary.r);
        expr.reg = self.new_reg();
        match unary.op {
          Operator::Neg => self.push(Tac::Neg(expr.reg, unary.r.reg)),
          Operator::Not => self.push(Tac::Not(expr.reg, unary.r.reg)),
          _ => unimplemented!(),
        }
      }
      Binary(binary) => {
        use ast::Operator::*;
        self.expr(&mut binary.l);
        self.expr(&mut binary.r);
        expr.reg = self.new_reg();
        let (l, r, d) = (binary.l.reg, binary.r.reg, expr.reg);
        match binary.op {
          Add => self.push(Tac::Add(d, l, r)),
          Sub => self.push(Tac::Sub(d, l, r)),
          Mul => self.push(Tac::Mul(d, l, r)),
          Div => self.push(Tac::Div(d, l, r)),
          Mod => self.push(Tac::Mod(d, l, r)),
          Lt => self.push(Tac::Lt(d, l, r)),
          Le => self.push(Tac::Le(d, l, r)),
          Gt => self.push(Tac::Gt(d, l, r)),
          Ge => self.push(Tac::Ge(d, l, r)),
          And => self.push(Tac::And(d, l, r)),
          Or => self.push(Tac::Or(d, l, r)),
          Eq | Ne => if binary.l.type_ == STRING {
            self.push(Tac::Param(l));
            self.push(Tac::Param(r));
            expr.reg = self.intrinsic_call(STRING_EQUAL);
            if op == Ne { self.push(Tac::Not(expr.reg, expr.reg)); }
          } else {
            self.push(if op == Eq { Tac::Eq(d, l, r) } else { Tac::Ne(d, l, r) });
          }
          Repeat => {
            unimplemented!();
          }
          _ => unimplemented!(),
        }
      }
      This => expr.reg = self.cur_this,
      ReadInt => expr.reg = self.intrinsic_call(READ_INT),
      ReadLine => expr.reg = self.intrinsic_call(READ_LINE),
      NewClass { name } => {
        expr.reg = self.new_reg();
        self.push(Tac::DirectCall(expr.reg, format!("_{}_New", name)));
      }
      NewArray { expr_t: _, len } => {
        expr.reg = self.new_reg();
      }
      TypeTest { src, name } => {
        // ans = 0
        // while (cur)
        //   if cur == target
        //     ans = 1
        //     break
        //   cur = cur->parent
        self.expr(src);
        expr.reg = self.new_reg();
        let (before_cond, after_body) = (self.new_label(), self.new_label());
        let (cur, target) = (self.new_reg(), self.new_reg());
        self.push(Tac::IntConst(expr.reg, 0));
        self.push(Tac::LoadVTbl(target, name));
        self.push(Tac::Load { dst: v_tbl, base: src.reg, offset: 0 });
        self.push(Tac::Label(before_cond));
        self.push(Tac::Je(cur, after_body));
        self.push(Tac::Eq(expr.reg, cur, target));
        self.push(Tac::Jne(expr.reg, after_body));
        self.push(Tac::Load { dst: cur, base: cur, offset: 0 });
        self.push(Tac::Jmp(before_cond));
        self.push(Tac::Label(after_body));
      }
      TypeCast { name, expr } => {
//        Label loop = Label.createLabel();
//        Label exit = Label.createLabel();
//        Temp cond = Temp.createTempI4();
//        Temp targetVp = genLoadVTable(c.getVtable());
//        Temp vp = genLoad(val, 0);
//        genMark(loop);
//        append(Tac.genEqu(cond, targetVp, vp));
//        genBnez(cond, exit);
//        append(Tac.genLoad(vp, vp, Temp.createConstTemp(0)));
//        genBnez(vp, loop);
//        Temp msg = genLoadStrConst(RuntimeError.CLASS_CAST_ERROR1);
//        genParm(msg);
//        genIntrinsicCall(Intrinsic.PRINT_STRING);
//        Temp instanceClassName = genLoad(genLoad(val, 0), 4);
//        genParm(instanceClassName);
//        genIntrinsicCall(Intrinsic.PRINT_STRING);
//        msg = genLoadStrConst(RuntimeError.CLASS_CAST_ERROR2);
//        genParm(msg);
//        genIntrinsicCall(Intrinsic.PRINT_STRING);
//        Temp targetClassName = genLoad(genLoadVTable(c.getVtable()), 4);
//        genParm(targetClassName);
//        genIntrinsicCall(Intrinsic.PRINT_STRING);
//        msg = genLoadStrConst(RuntimeError.CLASS_CAST_ERROR3);
//        genParm(msg);
//        genIntrinsicCall(Intrinsic.PRINT_STRING);
//        genIntrinsicCall(Intrinsic.HALT);
//        genMark(exit);
      }
      Range(_) => unimplemented!(),
      Default(default) => {
        expr.reg = self.new_reg();
      },
      Comprehension(_) => unimplemented!(),
    }
  }
}
