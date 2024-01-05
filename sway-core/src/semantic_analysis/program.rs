use crate::{
    language::{
        parsed::{ParseProgram, Expression, ExpressionKind, MatchExpression, MatchBranch, CodeBlock, Scrutinee, AsmExpression, AsmRegisterDeclaration, FunctionApplicationExpression, AstNode, AstNodeContent, IfExpression, Declaration, VariableDeclaration, IntrinsicFunctionExpression, FunctionDeclaration},
        ty::{self, TyProgram, TyFunctionDecl, TyDecl, TyAstNode}, Literal, AsmRegister, AsmOp, CallPath, Purity,
    },
    metadata::MetadataManager,
    semantic_analysis::{
        namespace::{self, Namespace},
        TypeCheckContext,
    },
    BuildConfig, Engines, TypeInfo, TypeBinding, TypeArgs, TypeArgument, transform::AttributesMap,
};
use sway_ast::Intrinsic;
use sway_error::handler::{ErrorEmitted, Handler};
use sway_ir::{Context, Module};
use sway_types::{Span, Ident, integer_bits::IntegerBits, Spanned};

use super::{
    TypeCheckAnalysis, TypeCheckAnalysisContext, TypeCheckFinalization,
    TypeCheckFinalizationContext,
};

impl TyProgram {
    /// Type-check the given parsed program to produce a typed program.
    ///
    /// The given `initial_namespace` acts as an initial state for each module within this program.
    /// It should contain a submodule for each library package dependency.
    pub fn type_check(
        handler: &Handler,
        engines: &Engines,
        parsed: &ParseProgram,
        initial_namespace: namespace::Module,
        package_name: &str,
        build_config: Option<&BuildConfig>,
    ) -> Result<Self, ErrorEmitted> {
        let mut namespace = Namespace::init_root(initial_namespace);
        let ctx =
            TypeCheckContext::from_root(&mut namespace, engines).with_kind(parsed.kind.clone());

        let mut ctx = if let Some(build_config) = build_config {
            ctx.with_experimental_flags(build_config.experimental)
        } else {
            ctx
        };

        let ParseProgram { root, kind } = parsed;

        // Analyze the dependency order for the submodules.
        let modules_dep_graph = ty::TyModule::analyze(handler, root)?;
        let module_eval_order = modules_dep_graph.compute_order(handler)?;

        dbg!(1);
        let mut m = ty::TyModule::type_check(handler, ctx.by_ref(), root, module_eval_order).and_then(|root| {
            let res = Self::validate_root(handler, engines, &root, kind.clone(), package_name);
            res.map(|(kind, declarations, configurables)| Self {
                kind,
                root,
                declarations,
                configurables,
                storage_slots: vec![],
                logged_types: vec![],
                messages_types: vec![],
            })
        })?;

        dbg!(1);

        if matches!(dbg!(&parsed.kind), crate::language::parsed::TreeType::Contract) {
            // /// Where 73 is the current offset in words from the start of the call frame.
            // const FIRST_PARAMETER_OFFSET: u64 = 73;
            // frame_ptr().add::<u64>(FIRST_PARAMETER_OFFSET).read()
            fn call_decode_first_param(engines: &Engines,) -> Expression {
                let string_slice_type_id = engines.te().insert(
                    engines,
                    TypeInfo::StringSlice,
                    None,
                );
                Expression {
                    kind: ExpressionKind::FunctionApplication(Box::new(
                        FunctionApplicationExpression {
                            call_path_binding: TypeBinding {
                                inner: CallPath {
                                    prefixes: vec![],
                                    suffix: Ident::new_no_span("decode_first_param".into()),
                                    is_absolute: false,
                                },
                                type_arguments: TypeArgs::Regular(vec![
                                    TypeArgument {
                                        type_id: string_slice_type_id,
                                        initial_type_id: string_slice_type_id,
                                        span: Span::dummy(),
                                        call_path_tree: None,
                                    },
                                ]),
                                span: Span::dummy(),
                            },
                            arguments: vec![
                            ],
                        },
                    )),
                    span: Span::dummy(),
                }
            }
            
            fn call_eq(engines: &Engines, l: Expression, r: Expression) -> Expression {
                let string_slice_type_id = engines.te().insert(
                    engines,
                    TypeInfo::Boolean,
                    None,
                );
                Expression {
                    kind: ExpressionKind::FunctionApplication(Box::new(
                        FunctionApplicationExpression {
                            call_path_binding: TypeBinding {
                                inner: CallPath {
                                    prefixes: vec![],
                                    suffix: Ident::new_no_span("eq".into()),
                                    is_absolute: false,
                                },
                                type_arguments: TypeArgs::Regular(vec![]),
                                span: Span::dummy(),
                            },
                            arguments: vec![l, r],
                        },
                    )),
                    span: Span::dummy(),
                }
            }

            let unit_type_id = engines.te().insert(
                engines,
                TypeInfo::Tuple(vec![]),
                None,
            );
            let string_slice_type_id = engines.te().insert(
                engines,
                TypeInfo::StringSlice,
                None,
            );

            let mut contents = vec![
                AstNode { 
                    content: AstNodeContent::Declaration(
                        Declaration::VariableDeclaration(
                            VariableDeclaration { 
                                name: Ident::new_no_span("method_name".to_string()), 
                                type_ascription: TypeArgument { 
                                    type_id: string_slice_type_id,
                                    initial_type_id: string_slice_type_id,
                                    span: Span::dummy(),
                                    call_path_tree: None
                                }, 
                                body: call_decode_first_param(engines), 
                                is_mutable: false
                            }
                        )
                    ),
                    span: Span::dummy()
                }
            ];

            let method_name_var_ref = Expression {
                kind: ExpressionKind::Variable(
                    Ident::new_no_span("method_name".to_string())
                ),
                span: Span::dummy(),
            };

            for (fn_decl, _) in  m.entry_fns(engines.de()) {
                contents.push(AstNode { 
                    content: AstNodeContent::Expression(
                        Expression {
                            kind: ExpressionKind::If(
                                IfExpression {
                                    // call eq
                                    condition: Box::new(
                                        call_eq(
                                            engines,
                                            method_name_var_ref.clone(),
                                            Expression {
                                                kind: ExpressionKind::Literal(
                                                    Literal::String(fn_decl.name.span())
                                                ),
                                                span: Span::dummy(),
                                            }
                                        )
                                    ),
                                    then: Box::new(
                                        Expression {
                                            kind: ExpressionKind::IntrinsicFunction(
                                                IntrinsicFunctionExpression {
                                                    name: Ident::new_no_span("__log".to_string()),
                                                    kind_binding: TypeBinding {
                                                        inner: Intrinsic::Log,
                                                        type_arguments: TypeArgs::Regular(vec![]),
                                                        span: Span::dummy(),
                                                    },
                                                    arguments: vec![method_name_var_ref.clone()],
                                                }
                                            ),
                                            span: Span::dummy(),
                                        }
                                    ),
                                    r#else: None,
                                }
                            ),
                            span: Span::dummy(),
                        }
                    ), 
                    span: Span::dummy()
                });
            }

            let entry_fn_decl = crate::language::parsed::function::FunctionDeclaration {
                purity: Purity::ReadsWrites,
                attributes: AttributesMap::default(),
                name: Ident::new_no_span("__entry".to_string()),
                visibility: crate::language::Visibility::Public,
                body: CodeBlock {
                    contents,
                    whole_block_span: Span::dummy(),
                },
                parameters: vec![],
                span: Span::dummy(),
                return_type: TypeArgument {
                    type_id: unit_type_id,
                    initial_type_id: unit_type_id,
                    span: Span::dummy(),
                    call_path_tree: None,
                },
                type_parameters: vec![],
                where_clause: vec![],
            };

            dbg!("__entry");
            m.root.all_nodes.push(
                TyAstNode::type_check (
                    handler,
                    ctx,
                    AstNode {
                        content: AstNodeContent::Declaration(
                             Declaration::FunctionDeclaration(entry_fn_decl)
                        ),
                        span: Span::dummy(),
                    }
                )?
            );

            // m.declarations.push(
            //     TyDecl::type_check(
            //         handler,
            //         ctx,
            //         Declaration::FunctionDeclaration(entry_fn_decl)
            //     )?
            // );
        }

        dbg!(1);

        Ok(m)
    }

    pub(crate) fn get_typed_program_with_initialized_storage_slots(
        self,
        handler: &Handler,
        engines: &Engines,
        context: &mut Context,
        md_mgr: &mut MetadataManager,
        module: Module,
    ) -> Result<Self, ErrorEmitted> {
        let decl_engine = engines.de();
        match &self.kind {
            ty::TyProgramKind::Contract { .. } => {
                let storage_decl = self
                    .declarations
                    .iter()
                    .find(|decl| matches!(decl, ty::TyDecl::StorageDecl { .. }));

                // Expecting at most a single storage declaration
                match storage_decl {
                    Some(ty::TyDecl::StorageDecl(ty::StorageDecl {
                        decl_id,
                        decl_span: _,
                        ..
                    })) => {
                        let decl = decl_engine.get_storage(decl_id);
                        let mut storage_slots = decl.get_initialized_storage_slots(
                            handler, engines, context, md_mgr, module,
                        )?;
                        // Sort the slots to standardize the output. Not strictly required by the
                        // spec.
                        storage_slots.sort();
                        Ok(Self {
                            storage_slots,
                            ..self
                        })
                    }
                    _ => Ok(Self {
                        storage_slots: vec![],
                        ..self
                    }),
                }
            }
            _ => Ok(Self {
                storage_slots: vec![],
                ..self
            }),
        }
    }
}

impl TypeCheckAnalysis for TyProgram {
    fn type_check_analyze(
        &self,
        handler: &Handler,
        ctx: &mut TypeCheckAnalysisContext,
    ) -> Result<(), ErrorEmitted> {
        for node in self.root.all_nodes.iter() {
            node.type_check_analyze(handler, ctx)?;
        }
        Ok(())
    }
}

impl TypeCheckFinalization for TyProgram {
    fn type_check_finalize(
        &mut self,
        handler: &Handler,
        ctx: &mut TypeCheckFinalizationContext,
    ) -> Result<(), ErrorEmitted> {
        handler.scope(|handler| {
            for node in self.root.all_nodes.iter_mut() {
                let _ = node.type_check_finalize(handler, ctx);
            }
            Ok(())
        })
    }
}
