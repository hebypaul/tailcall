package tailcall.runtime.transcoder

import caliban.Value
import caliban.parsing.SourceMapper
import caliban.parsing.adt.Definition.TypeSystemDefinition.TypeDefinition.{FieldDefinition, InputValueDefinition}
import caliban.parsing.adt.{
  Definition => CalibanDefinition,
  Directive,
  Document => CalibanDocument,
  Type => CalibanType,
}
import tailcall.runtime.internal.TValid
import tailcall.runtime.model.Blueprint

/**
 * Converts the blueprint into a the final output document.
 */
trait Blueprint2Document {

  final def toDocument(blueprint: Blueprint): TValid[Nothing, CalibanDocument] =
    TValid.succeed {
      CalibanDocument(List(CalibanDefinition
        .TypeSystemDefinition
        .SchemaDefinition(blueprint.schema.directives.map(toCalibanDirective(_)), blueprint.schema.query, blueprint.schema.mutation, blueprint.schema.subscription),
      ) ++
        blueprint.definitions.map {
          case Blueprint.ObjectTypeDefinition(name, fields, description, implements) => CalibanDefinition
            .TypeSystemDefinition.TypeDefinition.ObjectTypeDefinition(
              description,
              name,
              implements.map(tpe => CalibanType.NamedType(tpe.name, true)),
              Nil,
              fields.map(toCalibanField),
            )
          case Blueprint.InputObjectTypeDefinition(name, fields, description) => CalibanDefinition.TypeSystemDefinition
            .TypeDefinition.InputObjectTypeDefinition(description, name, Nil, fields.map(toCalibanInputValue))
          case Blueprint.ScalarTypeDefinition(name, directives, description) => CalibanDefinition.TypeSystemDefinition
            .TypeDefinition.ScalarTypeDefinition(description, name, directives.map(toCalibanDirective(_)))
          case Blueprint.EnumTypeDefinition(name, directives, description, values) => CalibanDefinition
            .TypeSystemDefinition.TypeDefinition.EnumTypeDefinition(
              description,
              name,
              directives.map(toCalibanDirective(_)),
              values.map(toCalibanEnumValue(_)),
            )
          case Blueprint.UnionTypeDefinition(name, directives, description, types) => CalibanDefinition
            .TypeSystemDefinition.TypeDefinition
            .UnionTypeDefinition(description, name, directives.map(toCalibanDirective(_)), types)
          case Blueprint.InterfaceTypeDefinition(name, fields, description) => CalibanDefinition.TypeSystemDefinition
            .TypeDefinition.InterfaceTypeDefinition(description, name, Nil, fields.map(toCalibanField))
        },
        SourceMapper.empty,
      )
    }

  final private def toCalibanDirective(directive: Blueprint.Directive): Directive = {
    Directive(
      directive.name,
      directive.arguments.map { case (key, value) => key -> Transcoder.toInputValue(value).getOrElse(Value.NullValue) },
    )
  }

  final private def toCalibanEnumValue(
                                        definition: Blueprint.EnumValueDefinition
                                      ): CalibanDefinition.TypeSystemDefinition.TypeDefinition.EnumValueDefinition =
    CalibanDefinition.TypeSystemDefinition.TypeDefinition
      .EnumValueDefinition(definition.description, definition.name, definition.directives.map(toCalibanDirective(_)))

  final private def toCalibanField(field: Blueprint.FieldDefinition): FieldDefinition = {
    val directives = field.directives.map(toCalibanDirective(_))
    FieldDefinition(
      field.description,
      field.name,
      field.args.map(toCalibanInputValue),
      toCalibanType(field.ofType),
      directives,
    )
  }

  final private def toCalibanInputValue(inputValue: Blueprint.InputFieldDefinition): InputValueDefinition =
    CalibanDefinition.TypeSystemDefinition.TypeDefinition.InputValueDefinition(
      inputValue.description,
      inputValue.name,
      toCalibanType(inputValue.ofType),
      inputValue.defaultValue.map(Transcoder.toInputValue(_).getOrElse(Value.NullValue)),
      Nil,
    )

  final private def toCalibanType(tpe: Blueprint.Type): CalibanType =
    tpe match {
      case Blueprint.NamedType(name, nonNull) => CalibanType.NamedType(name, nonNull)
      case Blueprint.ListType(ofType, nonNull) => CalibanType.ListType(toCalibanType(ofType), nonNull)
    }
}
